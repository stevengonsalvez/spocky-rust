# Replays an operation script against the pinned relay's PaseoRelay.Delivery.Writer and prints
# one raw transcript block per operation.
# Usage (inside the baseline project): mix run --no-start relay-writer-baseline.exs OPS OUT
#
# Operations (one per line, space separated; `#` lines and blank lines are skipped):
#   scenario NAME TIMEOUT_MS CONTROL_BYTES   a fresh destination and Writer.start/3
#   reserve S BYTES DL | write S TVAR OPCODE LEN DL   the client functions (DL: far | past)
#   reserve_raw S BYTES DL | write_raw S TVAR OPCODE LEN DL   GenServer.call without the client
#   control LEN        Writer.control from a new process
#   ack WVAR | timeout TVAR | close CODE | kill S | kill_dest
# A source is a process named S that runs its calls one at a time; an operation for a source
# whose call has not been answered is skipped (`skip`). Reservation tokens are named t1, t2, ...
# and frames the destination received w1, w2, ...
#
# Block: `> op`, `d ...` (what the destination received, in order), `r ...` (replies the sources
# got, in the order the Writer sent them), `state ...`. The Writer's own timers use deadlines far in the future; the
# reservation timeout and the destination or source exit are delivered as messages.

defmodule WriterBaseline do
  alias PaseoRelay.Delivery.Writer
  alias PaseoRelay.Delivery.Deadline

  def main([ops_path, out_path]) do
    lines =
      ops_path |> File.read!() |> String.split("\n", trim: true) |> Enum.reject(&String.starts_with?(&1, "#"))

    state = %{writer: nil, dest: nil, sources: %{}, busy: MapSet.new(), tokens: %{}, frames: %{}, counter: %{t: 0, w: 0, c: 0}, base: {0, 0, 0, 0}}

    {state, out} =
      Enum.reduce(lines, {state, []}, fn line, {state, out} ->
        {state, block} = op(String.split(line, " "), line, state)
        {state, [block | out]}
      end)

    stop_all(state)
    File.write!(out_path, out |> Enum.reverse() |> Enum.join("\n"))
  end

  defp op(["scenario", name, timeout, control_bytes], line, state) do
    stop_all(state)
    harness = self()
    dest = spawn(fn -> dest_loop(harness) end)
    {:ok, writer} = Writer.start(dest, String.to_integer(timeout), String.to_integer(control_bytes))
    1 = :erlang.trace(writer, true, [:receive, :send])
    state = %{state | writer: writer, dest: dest, sources: %{}, busy: MapSet.new(), tokens: %{}, frames: %{}, counter: %{t: 0, w: 0, c: 0}, base: metrics()}
    {state, "# " <> name <> "\n> " <> line <> "\n" <> state_line(state)}
  end

  defp op(["reserve", s, bytes, dl], line, state) do
    writer = state.writer
    bytes = String.to_integer(bytes)
    call(state, s, line, :reserve, fn -> Writer.reserve(writer, bytes, deadline(dl)) end)
  end

  defp op(["reserve_raw", s, bytes, dl], line, state) do
    writer = state.writer
    bytes = String.to_integer(bytes)
    call(state, s, line, :reserve, fn -> raw(writer, {:reserve, bytes, deadline(dl)}) end)
  end

  defp op(["write", s, tvar, opcode, len, dl], line, state) do
    writer = state.writer
    token = state.tokens[tvar]
    payload = String.duplicate("x", String.to_integer(len))
    call(state, s, line, :write, fn -> Writer.write(writer, token, String.to_atom(opcode), payload, deadline(dl)) end)
  end

  defp op(["write_raw", s, tvar, opcode, len, _dl], line, state) do
    writer = state.writer
    token = state.tokens[tvar]
    payload = String.duplicate("x", String.to_integer(len))
    call(state, s, line, :write, fn -> raw(writer, {:write, token, String.to_atom(opcode), payload}) end)
  end

  defp op(["control", len], line, state) do
    writer = state.writer
    count = state.counter.c + 1
    state = %{state | counter: %{state.counter | c: count}}
    payload = String.duplicate("c", String.to_integer(len))
    call(state, "c#{count}", line, :control, fn -> Writer.control(writer, payload) end)
  end

  defp op(["ack", wvar], line, state) do
    send(state.writer, {:written, state.frames[wvar] || make_ref()})
    settle(line, state)
  end

  defp op(["timeout", tvar], line, state) do
    send(state.writer, {:reservation_timeout, state.tokens[tvar]})
    settle(line, state)
  end

  defp op(["close", code], line, state) do
    Writer.close(state.writer, String.to_integer(code), "bye")
    settle(line, state)
  end

  defp op(["kill", s], line, state) do
    pid = state.sources[s]

    if pid != nil and Process.alive?(pid) do
      kill_source(s, pid, line, state)
    else
      {state, block(line, "skip", state)}
    end
  end

  defp op(["kill_dest"], line, state) do
    ref = Process.monitor(state.dest)
    Process.exit(state.dest, :kill)

    receive do
      {:DOWN, ^ref, :process, _pid, _reason} -> :ok
    end

    Process.sleep(3)
    settle(line, state)
  end

  defp kill_source(s, pid, line, state) do
    Process.exit(pid, :kill)
    ref = Process.monitor(pid)

    receive do
      {:DOWN, ^ref, :process, _pid, _reason} -> :ok
    end

    # The relay's own DOWN message is delivered in the same exit, not necessarily first.
    Process.sleep(3)
    state = %{state | busy: MapSet.delete(state.busy, s)}
    settle(line, state)
  end

  defp deadline("far"), do: Deadline.after_ms(1_000_000)
  defp deadline("past"), do: Deadline.after_ms(-1_000)

  defp raw(writer, message) do
    case GenServer.call(writer, message, 5_000) do
      :ok -> "ok"
      {:ok, token} -> {:token, token}
      {:error, reason} -> "error #{reason}"
    end
  catch
    :exit, _reason -> "exit"
  end

  defp call(state, s, line, kind, fun) do
    state = ensure_source(state, s)

    if MapSet.member?(state.busy, s) do
      {state, block(line, "skip", state)}
    else
      pid = state.sources[s]
      state = %{state | busy: MapSet.put(state.busy, s)}
      send(pid, {:run, kind, fun})

      immediate =
        case await_call(state.writer, s, pid) do
          :called -> []
          :answered -> [s]
        end

      settle(line, state, immediate)
    end
  end

  defp ensure_source(state, s) do
    if Map.has_key?(state.sources, s) and Process.alive?(state.sources[s]) do
      state
    else
      harness = self()
      pid = spawn(fn -> source_loop(s, harness) end)
      %{state | sources: Map.put(state.sources, s, pid)}
    end
  end

  # What a source did: it runs one call at a time and reports the result.
  defp source_loop(name, harness) do
    receive do
      {:run, kind, fun} ->
        result = fun.()
        send(harness, {:done, name, kind, result})
        source_loop(name, harness)

      {:barrier, ref} ->
        send(harness, {:echo, name, ref})
        source_loop(name, harness)
    end
  end

  defp dest_loop(harness) do
    receive do
      {:relay_frame, _writer, ref, opcode, payload} ->
        send(harness, {:dest, {:frame, ref, opcode, byte_size(payload), payload}})
        dest_loop(harness)

      {:relay_write_barrier, _writer, ref} ->
        send(harness, {:dest, {:barrier, ref}})
        dest_loop(harness)

      {:relay_close, code, reason} ->
        send(harness, {:dest, {:close, code, reason}})
        dest_loop(harness)

      {:barrier, ref} ->
        send(harness, {:echo, :dest, ref})
        dest_loop(harness)
    end
  end

  # Wait until the Writer has processed everything sent so far and every process of the
  # harness has forwarded what it received; a source that does not answer is waiting for a reply.
  # Wait until the source's call has reached the Writer (the trace of what the Writer receives
  # shows it) or the source answered without one: the client functions return `timeout` for an
  # expired deadline and `destination_closed` for a stopped Writer without a message.
  defp await_call(writer, name, pid) do
    receive do
      {:trace, ^writer, :receive, {:"$gen_call", {^pid, tag}, _request}} ->
        # The reply is sent to `tag` (an alias), so remember whose call it was.
        Process.put({:call_tag, tag}, pid)
        :called

      {:done, ^name, _kind, _result} = done ->
        send(self(), done)
        :answered
    after
      5_000 -> raise "source #{name} neither called the Writer nor answered"
    end
  end

  # The processes the Writer replied to, from its send trace, in order.
  defp reply_targets(writer, acc \\ []) do
    receive do
      {:trace, ^writer, :send, {tag, _reply}, _to} when tag != nil ->
        case Process.get({:call_tag, tag}) do
          nil -> reply_targets(writer, acc)
          pid -> reply_targets(writer, [pid | acc])
        end

      {:trace, ^writer, _kind, _message} ->
        reply_targets(writer, acc)

      {:trace, ^writer, :send, _message, _to} ->
        reply_targets(writer, acc)
    after
      0 -> Enum.reverse(acc)
    end
  end

  # Replies are sent while the Writer handles a message, so after a sync the trace lists every
  # source that will answer; a Writer that stopped answers every caller still waiting (a reply,
  # or the exit of its call).
  defp settle(line, state, immediate \\ []) do
    sync_writer(state.writer)
    stopped = not Process.alive?(state.writer)
    targets = reply_targets(state.writer)
    names = Map.new(state.sources, fn {name, pid} -> {pid, name} end)

    replied = for pid <- targets, name = names[pid], Process.alive?(pid), do: name

    awaited =
      if stopped do
        replied ++ (for name <- state.busy, Process.alive?(state.sources[name]), do: name)
      else
        replied ++ immediate
      end

    {state, results} = await_dones(Enum.uniq(awaited), state, [])
    ordered = Enum.uniq(awaited)

    if Process.alive?(state.dest) do
      ref = make_ref()
      send(state.dest, {:barrier, ref})

      receive do
        {:echo, :dest, ^ref} -> :ok
      after
        5_000 -> raise "the destination did not answer the barrier"
      end
    end

    {state, dest} = collect_dest(state, [])
    {state, results} = sweep_dones(state, results)
    # The replies in the order the Writer sent them; callers that got an exit instead of a reply
    # (no send to order) follow, by name.
    replies =
      Enum.flat_map(ordered, fn name -> for {^name, text} <- results, do: text end) ++
        (for {name, text} <- Enum.sort(results), name not in ordered, do: text)

    {state, block(line, {dest, replies}, state)}
  end

  defp await_dones([], state, replies), do: {state, replies}

  defp await_dones(awaited, state, replies) do
    receive do
      {:done, name, kind, result} ->
        {state, text} = reply_text(state, name, kind, result)
        state = %{state | busy: MapSet.delete(state.busy, name)}
        await_dones(List.delete(awaited, name), state, [{name, text} | replies])
    after
      5_000 -> raise "no answer from #{inspect(awaited)}"
    end
  end

  defp sweep_dones(state, replies) do
    receive do
      {:done, name, kind, result} ->
        {state, text} = reply_text(state, name, kind, result)
        sweep_dones(%{state | busy: MapSet.delete(state.busy, name)}, [{name, text} | replies])
    after
      0 -> {state, replies}
    end
  end

  defp collect_dest(state, dest) do
    receive do
      {:dest, event} ->
        {state, text} = dest_text(state, event)
        collect_dest(state, [text | dest])
    after
      0 -> {state, Enum.reverse(dest)}
    end
  end

  # The Writer may stop while this call is in flight.
  # A source that is already dead when it is monitored sends its exit message back at once, so
  # sync until the Writer's mailbox stays empty.
  defp sync_writer(writer), do: sync_writer(writer, 10)

  defp sync_writer(writer, rounds) do
    state = :sys.get_state(writer)

    case Process.info(writer, :message_queue_len) do
      {:message_queue_len, 0} -> state
      _busy when rounds > 0 -> sync_writer(writer, rounds - 1)
      _busy -> state
    end
  catch
    :exit, _reason -> :stopped
  end

  defp dest_text(state, {:frame, ref, opcode, size, payload}) do
    count = state.counter.w + 1
    name = "w#{count}"
    state = %{state | counter: %{state.counter | w: count}, frames: Map.put(state.frames, name, ref)}
    {state, "frame #{name} #{opcode} #{size} #{inspect(payload)}"}
  end

  defp dest_text(state, {:barrier, ref}), do: {state, "barrier #{frame_name(state, ref)}"}
  defp dest_text(state, {:close, code, reason}), do: {state, "close #{code} #{reason}"}

  defp frame_name(state, ref), do: Enum.find_value(state.frames, "?", fn {name, r} -> if r == ref, do: name end)

  defp reply_text(state, name, kind, {:token, token}) do
    count = state.counter.t + 1
    token_name = "t#{count}"
    state = %{state | counter: %{state.counter | t: count}, tokens: Map.put(state.tokens, token_name, token)}
    {state, "#{name} #{kind} ok #{token_name}"}
  end

  defp reply_text(state, name, kind, {:ok, token}), do: reply_text(state, name, kind, {:token, token})
  defp reply_text(state, name, kind, :ok), do: {state, "#{name} #{kind} ok"}
  defp reply_text(state, name, kind, {:error, reason}), do: {state, "#{name} #{kind} error #{reason}"}
  defp reply_text(state, name, kind, text) when is_binary(text), do: {state, "#{name} #{kind} #{text}"}

  defp block(line, {dest, replies}, state) do
    ["> " <> line] ++
      if(dest == [], do: [], else: ["d " <> Enum.join(dest, " | ")]) ++
      if(replies == [], do: [], else: ["r " <> Enum.join(replies, " | ")]) ++ [state_line(state)]
      |> Enum.join("\n")
  end

  defp block(line, text, state) when is_binary(text), do: Enum.join(["> " <> line, "= " <> text, state_line(state)], "\n")

  defp state_line(state) do
    {alive, active, queued, control, live} =
      if Process.alive?(state.writer) and is_map(sync_writer(state.writer)) do
        w = :sys.get_state(state.writer)
        names = Map.new(state.sources, fn {name, pid} -> {pid, name} end)
        tokens = Map.new(state.tokens, fn {name, ref} -> {ref, name} end)

        active =
          case w.active do
            nil -> "none"
            %{kind: :control} -> "control"
            %{token: token} -> "payload:" <> Map.get(tokens, token, "?")
          end

        queued =
          w.queued
          |> :queue.to_list()
          |> Enum.map_join(",", fn
            %{kind: :payload, from: {pid, _tag}, bytes: bytes} -> "p:#{Map.get(names, pid, "?")}:#{bytes}"
            %{kind: :control, bytes: bytes} -> "c:#{bytes}"
          end)

        {:monitors, monitors} = Process.info(state.writer, :monitors)

        timers =
          case w.active do
            %{timer: timer} -> if is_integer(Process.read_timer(timer)), do: 1, else: 0
            nil -> 0
          end

        {true, active, queued, w.queued_control_bytes, "#{length(monitors)},#{timers}"}
      else
        {false, "-", "-", 0, "-"}
      end

    {frames, bytes, timeouts, slow} = metrics()
    {f0, b0, t0, s0} = state.base

    "state alive=#{alive} active=#{active} queued=#{queued} control=#{control} live=#{live} " <>
      "metrics=#{frames - f0},#{bytes - b0},#{timeouts - t0},#{slow - s0}"
  end

  defp metrics do
    {PaseoRelay.Metrics.value(:frames_forwarded), PaseoRelay.Metrics.value(:bytes_forwarded),
     PaseoRelay.Metrics.value(:delivery_timeouts), PaseoRelay.Metrics.value(:slow_consumer_disconnects)}
  end

  defp stop_all(%{writer: nil}), do: :ok

  defp stop_all(state) do
    if Process.alive?(state.writer), do: Process.exit(state.writer, :kill)
    if Process.alive?(state.dest), do: Process.exit(state.dest, :kill)
    Enum.each(state.sources, fn {_name, pid} -> Process.exit(pid, :kill) end)
  end
end

WriterBaseline.main(System.argv())
