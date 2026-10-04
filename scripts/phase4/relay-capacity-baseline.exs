# Replays an operation script against the pinned relay's PaseoRelay.Capacity and prints one raw
# transcript block per operation.
# Usage (inside the baseline project): mix run --no-start relay-capacity-baseline.exs OPS OUT
#
# Operations (one per line, space separated; `#` lines and blank lines are skipped):
#   scenario NAME BUDGET WEIGHT WATERMARK   restart Capacity with this configuration
#   spawn P | kill P                         a fake socket process, and its exit
#   admit CALLER NS LIMIT HOLDER             admit_connection, the token is named c1, c2, ...
#   attach CALLER CVAR | release CALLER CVAR | expire CVAR
#   msg CALLER BYTES                         admit_message, the token is named m1, m2, ...
#   start CALLER MVAR | finish CALLER MVAR | cancel CALLER MVAR
#   check_now | check | recheck              shed_if_needed through call, :check, :pressure_recheck
#   set_watermark_rel DELTA | set_watermark_abs BYTES
#   status NS LIMIT | active NS | value NAME
#
# Block: `> op`, `= reply`, `~ input` (the memory reading or watermark the relay used, an input
# the Rust replay is given), `@ messages` (sent to fake processes, sorted by name), `state ...`.
# Real timers of the relay (5 s reservations, 1 s check, 100 ms recheck) stay armed; a scenario
# runs in a few milliseconds and finishes with the watermark disabled where pressure was set.

defmodule CapacityBaseline do
  alias PaseoRelay.Capacity

  @timeout 5_000

  def main([ops_path, out_path]) do
    lines =
      ops_path
      |> File.read!()
      |> String.split("\n", trim: true)
      |> Enum.reject(&String.starts_with?(&1, "#"))

    harness = self()
    state = %{procs: %{}, tokens: %{}, counters: %{c: 0, m: 0}, metrics_base: {0, 0}, harness: harness}

    {_state, out} =
      Enum.reduce(lines, {state, []}, fn line, {state, out} ->
        {state, block} = op(String.split(line, " "), line, state)
        {state, [block | out]}
      end)

    stop_capacity()
    File.write!(out_path, out |> Enum.reverse() |> Enum.join("\n"))
  end

  defp op(["scenario", name, budget, weight, watermark], line, state) do
    stop_capacity()
    Enum.each(state.procs, fn {_name, pid} -> Process.exit(pid, :kill) end)
    config = %{
      ingress_budget_bytes: String.to_integer(budget),
      ingress_weight: String.to_integer(weight),
      memory_watermark_bytes: String.to_integer(watermark)
    }

    {:ok, _pid} = Capacity.start_link(config)
    Process.unlink(Process.whereis(Capacity))
    base = {PaseoRelay.Metrics.value(:memory_pressure_disconnects), PaseoRelay.Metrics.value(:delivery_wait_count)}
    state = %{state | procs: %{}, tokens: %{}, counters: %{c: 0, m: 0}, metrics_base: base}
    {state, ["# " <> name, "> " <> line, "= ok", state_line(state)] |> Enum.join("\n")}
  end

  defp op(["spawn", name], line, state) do
    harness = state.harness
    pid = spawn(fn -> fake(name, harness) end)
    state = %{state | procs: Map.put(state.procs, name, pid)}
    {state, block(line, "ok", [], [], state)}
  end

  defp op(["kill", name], line, state) do
    pid = state.procs[name]
    ref = Process.monitor(pid)
    Process.exit(pid, :kill)

    receive do
      {:DOWN, ^ref, :process, _pid, _reason} -> :ok
    end

    wait_released(pid, 1_000)
    {state, block(line, "ok", [], [], state)}
  end

  defp op(["admit", caller, namespace, limit, holder], line, state) do
    holder_pid = state.procs[holder]
    result =
      run(state, caller, fn ->
        Capacity.admit_connection(namespace, String.to_integer(limit), holder_pid, @timeout)
      end)

    reply_token(result, :c, line, state)
  end

  defp op(["attach", caller, cvar], line, state) do
    token = state.tokens[cvar]
    result = run(state, caller, fn -> Capacity.attach_connection(token, @timeout) end)
    {state, block(line, reply(result, state), [], [], state)}
  end

  defp op(["release", caller, cvar], line, state) do
    token = state.tokens[cvar]
    run(state, caller, fn -> Capacity.release_connection(token) end)
    sync()
    {state, block(line, "ok", [], [], state)}
  end

  defp op(["expire", cvar], line, state) do
    send(Process.whereis(Capacity), {:expire, state.tokens[cvar]})
    sync()
    {state, block(line, "ok", [], [], state)}
  end

  defp op(["msg", caller, bytes], line, state) do
    result = run(state, caller, fn -> Capacity.admit_message(String.to_integer(bytes), @timeout) end)
    reply_token(result, :m, line, state)
  end

  defp op(["start", caller, mvar], line, state) do
    token = state.tokens[mvar]
    result = run(state, caller, fn -> Capacity.start_delivery(token, @timeout) end)
    {state, block(line, reply(result, state), [], [], state)}
  end

  defp op([kind, caller, mvar], line, state) when kind in ["finish", "cancel"] do
    token = state.tokens[mvar]

    run(state, caller, fn ->
      if kind == "finish", do: Capacity.finish_message(token), else: Capacity.cancel_message(token)
    end)

    sync()
    {state, block(line, "ok", [], [], state)}
  end

  defp op(["check_now"], line, state) do
    memory = :erlang.memory(:total)
    :ok = Capacity.check_now(@timeout)
    shed(line, memory, state)
  end

  defp op(["check"], line, state) do
    memory = :erlang.memory(:total)
    send(Process.whereis(Capacity), :check)
    sync()
    shed(line, memory, state)
  end

  defp op(["recheck"], line, state) do
    memory = :erlang.memory(:total)
    send(Process.whereis(Capacity), :pressure_recheck)
    sync()
    shed(line, memory, state)
  end

  defp op(["set_watermark_rel", delta], line, state) do
    watermark = :erlang.memory(:total) + String.to_integer(delta)
    :ok = Capacity.set_watermark(watermark, @timeout)
    {state, block(line, "ok", ["watermark=#{watermark}"], [], state)}
  end

  defp op(["set_watermark_abs", bytes], line, state) do
    :ok = Capacity.set_watermark(String.to_integer(bytes), @timeout)
    {state, block(line, "ok", [], [], state)}
  end

  defp op(["status", namespace, limit], line, state) do
    {:available, %{admission: admission, gauges: gauges}} = Capacity.status(namespace, String.to_integer(limit))
    reply = "available #{admission} #{gauge_text(gauges)}"
    {state, block(line, reply, [], [], state)}
  end

  defp op(["active", namespace], line, state) do
    {state, block(line, Integer.to_string(Capacity.active_connections(namespace)), [], [], state)}
  end

  defp op(["value", name], line, state) do
    {state, block(line, Integer.to_string(Capacity.value(String.to_atom(name))), [], [], state)}
  end

  # The relay sends `victims` messages in this call; the fake processes forward them.
  defp shed(line, before, state) do
    {memory, expected} =
      case :sys.get_state(Capacity).pressure do
        %{memory: used, victims: victims} -> {used, victims}
        nil -> {before, 0}
      end

    {state, block(line, "ok", ["memory=#{memory}"], received(expected), state)}
  end

  # A DOWN message reaches the relay after the harness sees it; wait until the relay has
  # dropped every connection the process held.
  defp wait_released(pid, remaining) do
    holders = :sys.get_state(Capacity).connections |> Map.values() |> Enum.map(& &1.holder)

    cond do
      pid not in holders -> :ok
      remaining == 0 -> raise "relay never saw the exit of #{inspect(pid)}"
      true ->
        Process.sleep(1)
        wait_released(pid, remaining - 1)
    end
  end

  defp reply_token({:ok, token}, kind, line, state) do
    count = state.counters[kind] + 1
    name = "#{kind}#{count}"
    state = %{state | tokens: Map.put(state.tokens, name, token), counters: Map.put(state.counters, kind, count)}
    {state, block(line, "ok #{name}", [], [], state)}
  end

  defp reply_token({:error, reason}, _kind, line, state),
    do: {state, block(line, "error #{reason}", [], [], state)}

  defp reply(:ok, _state), do: "ok"
  defp reply({:ok, _pid}, _state), do: "ok"
  defp reply({:error, reason}, _state), do: "error #{reason}"

  defp block(line, reply, inputs, messages, state) do
    ["> " <> line, "= " <> reply] ++
      Enum.map(inputs, &("~ " <> &1)) ++
      if(messages == [], do: [], else: ["@ " <> Enum.join(messages, " ")]) ++
      [state_line(state)]
      |> Enum.join("\n")
  end

  defp state_line(state) do
    capacity = :sys.get_state(Capacity)
    names = Map.new(state.procs, fn {name, pid} -> {pid, name} end)
    name = fn pid -> Map.get(names, pid, "?") end
    pressure =
      case capacity.pressure do
        nil -> "none"
        %{victims: victims, batch: batch} -> "#{victims}/#{batch}"
      end

    sizes =
      Enum.join(
        [
          map_size(capacity.namespaces),
          map_size(capacity.connections),
          map_size(capacity.sockets),
          map_size(capacity.messages),
          map_size(capacity.monitors)
        ],
        ","
      )

    {mpd, dwc} = state.metrics_base
    metrics =
      "#{PaseoRelay.Metrics.value(:memory_pressure_disconnects) - mpd},#{PaseoRelay.Metrics.value(:delivery_wait_count) - dwc}"

    "state gauges=#{gauge_text(Capacity.snapshot())} pressure=#{pressure} " <>
      "active=#{capacity.active |> :gb_trees.values() |> Enum.map_join(",", name)} " <>
      "blocked=#{capacity.blocked |> :gb_trees.values() |> Enum.map_join(",", name)} " <>
      "sizes=#{sizes} metrics=#{metrics}"
  end

  defp gauge_text(gauges) do
    Enum.map_join(
      [:active_websockets, :ingress_reserved_bytes, :inflight_delivery_bytes, :backpressured_sources],
      ",",
      &Integer.to_string(Map.fetch!(gauges, &1))
    )
  end

  defp received(expected), do: collect([], expected) |> Enum.sort()

  defp collect(acc, 0), do: acc

  defp collect(acc, remaining) do
    receive do
      {:got, name, message} -> collect(["#{name}:#{message}" | acc], remaining - 1)
    after
      1_000 -> raise "fake processes forwarded #{length(acc)} of the expected messages"
    end
  end

  defp run(state, caller, fun) do
    pid = state.procs[caller]
    ref = make_ref()
    send(pid, {:run, ref, fun, self()})

    receive do
      {:ran, ^ref, result} -> result
    end
  end

  defp sync, do: :sys.get_state(Capacity)

  defp stop_capacity do
    case Process.whereis(Capacity) do
      nil -> :ok
      pid -> GenServer.stop(pid)
    end
  end

  defp fake(name, harness) do
    receive do
      {:run, ref, fun, from} ->
        send(from, {:ran, ref, fun.()})
        fake(name, harness)

      :relay_memory_pressure ->
        send(harness, {:got, name, "relay_memory_pressure"})
        fake(name, harness)

      _other ->
        fake(name, harness)
    end
  end
end

CapacityBaseline.main(System.argv())
