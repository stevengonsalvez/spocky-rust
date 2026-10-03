# Drives the running pinned relay over real sockets and prints the raw wire it answers with.
# Usage (inside the baseline project, test env): mix run relay-protocol-live.exs OUT GENERATED
# Masked in OUT: ts (wall_clock) and a generated v2 connection id (generated_id), nothing else.
# GENERATED receives the unmasked frames of the generated-id sync case: its ids are random,
# so it is replayed by the Rust test against the encoder instead of being diffed.

defmodule Live.Client do
  use WebSockex

  def start(url, owner), do: WebSockex.start(url, __MODULE__, owner)

  def handle_connect(_connection, owner) do
    send(owner, {:open, self()})
    {:ok, owner}
  end

  def handle_frame({kind, payload}, owner) do
    send(owner, {:frame, self(), kind, payload})
    {:ok, owner}
  end

  def handle_disconnect(%{reason: reason}, owner) do
    send(owner, {:closed, self(), reason})
    {:ok, owner}
  end
end

defmodule Live do
  @port 4000

  def url(query), do: "ws://127.0.0.1:#{@port}/ws?#{query}"

  def connect(query) do
    {:ok, client} = Live.Client.start(url(query), self())

    receive do
      {:open, ^client} -> client
    after
      2_000 -> raise "no open for #{query}"
    end
  end

  def recv(client, timeout \\ 5_000) do
    receive do
      {:frame, ^client, kind, payload} -> {kind, payload}
      {:closed, ^client, {:remote, code, reason}} -> {:close, code, reason}
      {:closed, ^client, other} -> {:closed, inspect(other)}
    after
      timeout -> :none
    end
  end

  def drain(client, label, emit, show \\ &show/1) do
    case recv(client, 300) do
      :none -> :ok
      other ->
        emit.("#{label}\t" <> show.(other))
        drain(client, label, emit, show)
    end
  end

  # ts is wall_clock; every frame that can carry a time masks it.
  def show({kind, payload}) when kind in [:text, :binary],
    do: "#{kind} " <> String.replace(payload, ~r/"ts":\d+/, ~s("ts":<wall_clock>))

  def show({:close, code, reason}), do: "close #{code} #{inspect(reason)}"
  def show(other), do: inspect(other)

  # A generated v2 connection id is generated_id: exactly conn_ and 16 lowercase hex digits.
  def show_generated(value),
    do: String.replace(show(value), ~r/conn_[0-9a-f]{16}/, "<generated_id>")

  def http(request) do
    {:ok, socket} = :gen_tcp.connect({127, 0, 0, 1}, @port, [:binary, active: false])
    :ok = :gen_tcp.send(socket, request)
    response = read_all(socket, "")
    :gen_tcp.close(socket)
    response
  end

  defp read_all(socket, acc) do
    case :gen_tcp.recv(socket, 0, 1_000) do
      {:ok, data} -> read_all(socket, acc <> data)
      {:error, _} -> acc
    end
  end

  def upgrade_request(target) do
    "GET #{target} HTTP/1.1\r\nhost: 127.0.0.1\r\nconnection: Upgrade\r\nupgrade: websocket\r\n" <>
      "sec-websocket-version: 13\r\nsec-websocket-key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
  end

  def plain_request(target), do: "GET #{target} HTTP/1.1\r\nhost: 127.0.0.1\r\nconnection: close\r\n\r\n"

  def status_and_body(response) do
    [head, body] = String.split(response, "\r\n\r\n", parts: 2)
    [status | _] = String.split(head, "\r\n")
    headers = head |> String.split("\r\n") |> tl() |> Enum.reject(&String.starts_with?(&1, "date:"))
    "#{status} headers=#{inspect(headers)} body=#{inspect(body)}"
  end
end

[output, generated_output] = System.argv()
unless Node.alive?() do
  {_, 0} = System.cmd("epmd", ["-daemon"])
  {:ok, _} = Node.start(:relay_protocol_live, :shortnames)
end

lines = :ets.new(:lines, [:ordered_set, :public])
counter = :counters.new(1, [])
emit = fn text ->
  :counters.add(counter, 1, 1)
  :ets.insert(lines, {:counters.get(counter, 1), text})
end

# HTTP rejections
for {name, target, upgrade} <- [
      {"no_upgrade", "/ws?serverId=s&role=server", false},
      {"no_role", "/ws?serverId=s", true},
      {"bad_role", "/ws?serverId=s&role=x", true},
      {"no_server_id", "/ws?role=server", true},
      {"empty_server_id", "/ws?role=server&serverId=", true},
      {"long_server_id", "/ws?role=server&serverId=" <> String.duplicate("a", 257), true},
      {"bad_version", "/ws?role=server&serverId=s&v=3", true},
      {"long_connection_id", "/ws?role=server&serverId=s&v=2&connectionId=" <> String.duplicate("a", 257), true},
      {"bad_percent", "/ws?role=server&serverId=%zz", true},
      {"empty_name", "/ws?=x&role=server&serverId=s", true},
      {"too_many_keys", "/ws?" <> Enum.map_join(1..101, "&", &"k#{&1}=v") <> "&role=server&serverId=s", true}
    ] do
  request = if upgrade, do: Live.upgrade_request(target), else: Live.plain_request(target)
  emit.("http\t#{name}\t" <> Live.status_and_body(Live.http(request)))
end

# v2 control, clients, data
control = Live.connect("serverId=live_a&role=server&v=2")
emit.("control_first\t" <> Live.show(Live.recv(control)))
client = Live.connect("serverId=live_a&role=client&v=2&connectionId=clt_a")
emit.("client_connected\t" <> Live.show(Live.recv(control)))
WebSockex.send_frame(control, {:text, ~s({"type":"ping"})})
emit.("pong\t" <> Live.show(Live.recv(control)))
WebSockex.send_frame(client, {:text, "before-data"})
data = Live.connect("serverId=live_a&role=server&v=2&connectionId=clt_a")
emit.("buffered_to_data\t" <> Live.show(Live.recv(data)))
generated = Live.connect("serverId=live_a&role=client&v=2")
emit.("generated_connected\t" <> Live.show_generated(Live.recv(control)))

control2 = Live.connect("serverId=live_a&role=server&v=2")
emit.("control_replaced_old\t" <> Live.show(Live.recv(control)))
emit.("control_replaced_new_sync\t" <> Live.show_generated(Live.recv(control2)))

WebSockex.cast(client, :close)
Process.sleep(200)
emit.("client_left_control\t" <> Live.show(Live.recv(control2)))
emit.("client_left_data\t" <> Live.show(Live.recv(data)))
WebSockex.cast(generated, :close)
Process.sleep(200)
Live.drain(control2, "control_late", emit, &Live.show_generated/1)

# data replaced and server disconnect
client_b = Live.connect("serverId=live_a&role=client&v=2&connectionId=clt_b")
emit.("client_b_connected\t" <> Live.show(Live.recv(control2)))
data_b = Live.connect("serverId=live_a&role=server&v=2&connectionId=clt_b")
data_b2 = Live.connect("serverId=live_a&role=server&v=2&connectionId=clt_b")
emit.("data_replaced\t" <> Live.show(Live.recv(data_b)))
WebSockex.cast(data_b2, :close)
Process.sleep(200)
emit.("server_disconnected\t" <> Live.show(Live.recv(client_b)))
Live.drain(control2, "control_late", emit)

# ids that need JSON escaping
for {name, id} <- [
      {"quote_backslash", "a%22b%5Cc"},
      {"control_chars", "a%01%0Ab%1Fc"},
      {"unicode", "%C3%A9%E2%82%AC%F0%9F%98%80"},
      {"line_separator", "x%E2%80%A8y"},
      {"slash", "a%2Fb"},
      {"trim_nbsp", "%C2%A0trimmed%C2%A0"}
    ] do
  c = Live.connect("serverId=live_a&role=client&v=2&connectionId=#{id}")
  emit.("escape_#{name}\t" <> Live.show(Live.recv(control2)))
  WebSockex.cast(c, :close)
  emit.("escape_#{name}_left\t" <> Live.show(Live.recv(control2)))
end

# an identifier Jason cannot encode
bad = Live.connect("serverId=live_a&role=client&v=2&connectionId=%FF")
emit.("invalid_utf8_control\t" <> Live.show(Live.recv(control2, 2_000)))
emit.("invalid_utf8_client\t" <> Live.show(Live.recv(bad, 2_000)))
emit.("invalid_utf8_control_after\t" <> Live.show(Live.recv(control2, 500)))

# sync order beyond a small map: ids of every length class the hash loop treats differently
sync_case = fn label, server, ids ->
  first = Live.connect("serverId=#{server}&role=server&v=2")
  _ = Live.recv(first)

  clients =
    for id <- ids do
      client = Live.connect("serverId=#{server}&role=client&v=2&connectionId=#{URI.encode_www_form(id)}")
      _ = Live.recv(first)
      client
    end

  second = Live.connect("serverId=#{server}&role=server&v=2")
  emit.("#{label}\t" <> Live.show(Live.recv(second, 3_000)))
  {clients, second}
end

hex16 = fn i -> i |> Integer.to_string(16) |> String.pad_leading(16, "0") |> String.downcase() end
numbered = fn prefix, length -> for i <- 1..40, do: String.pad_trailing("#{prefix}#{i}", length, "x") end

for {label, ids} <- [
      {"sync_40", for(i <- 1..40, do: "conn_#{i}")},
      {"sync_conn21", for(i <- 1..40, do: "conn_" <> hex16.(i * 7919))},
      {"sync_id16", numbered.("i", 16)},
      {"sync_id32", numbered.("j", 32)},
      {"sync_id255", for(i <- 1..34, do: String.pad_trailing("k#{i}", 255, "y"))},
      {"sync_tails", for(length <- [12, 13, 14, 15, 17, 24, 28, 31, 32, 33, 48, 64], i <- 1..3, do: String.pad_trailing("t#{length}_#{i}", length, "z"))},
      {"sync_nonascii", for(i <- 1..40, do: "é€😀#{i}")}
    ] do
  {clients, second} = sync_case.(label, "live_#{label}", ids)
  Enum.each(clients, &WebSockex.cast(&1, :close))
  WebSockex.cast(second, :close)
  Process.sleep(200)
end

# the disconnect path: the client map grows past 32 keys, then shrinks back
{shrink_clients, shrink_control} = sync_case.("sync_33", "live_shrink", for(i <- 1..33, do: "conn_" <> hex16.(i * 104_729)))
[gone | rest] = shrink_clients
WebSockex.cast(gone, :close)
Process.sleep(300)
shrink_second = Live.connect("serverId=live_shrink&role=server&v=2")
emit.("sync_32_after_disconnect\t" <> Live.show(Live.recv(shrink_second, 3_000)))
[gone2 | rest] = rest
WebSockex.cast(gone2, :close)
Process.sleep(300)
shrink_third = Live.connect("serverId=live_shrink&role=server&v=2")
emit.("sync_31_after_disconnect\t" <> Live.show(Live.recv(shrink_third, 3_000)))
Enum.each(rest, &WebSockex.cast(&1, :close))
_ = shrink_control

# generated ids: random, so recorded unmasked in GENERATED
generated_lines = :ets.new(:generated, [:ordered_set, :public])
gen_first = Live.connect("serverId=live_generated&role=server&v=2")
_ = Live.recv(gen_first)

gen_clients =
  for i <- 1..40 do
    client = Live.connect("serverId=live_generated&role=client&v=2")
    {:text, frame} = Live.recv(gen_first)
    :ets.insert(generated_lines, {i, "connected\t" <> frame})
    client
  end

gen_second = Live.connect("serverId=live_generated&role=server&v=2")
{:text, sync_frame} = Live.recv(gen_second, 3_000)
:ets.insert(generated_lines, {41, "sync\t" <> sync_frame})
File.write!(generated_output, :ets.tab2list(generated_lines) |> Enum.map(fn {_, text} -> [text, "\n"] end))
Enum.each(gen_clients, &WebSockex.cast(&1, :close))

# handshake validation closes the client with 1008
public_key = :binary.copy(<<7>>, 32)
hello = fn type, key -> Jason.encode!(%{type: type, key: key, capabilities: %{}}) end
v1_daemon = Live.connect("serverId=live_v1&role=server")
v1_client = Live.connect("serverId=live_v1&role=client")
WebSockex.send_frame(v1_client, {:text, hello.("hello", Base.encode64(public_key))})
emit.("handshake_valid_forwarded\t" <> Live.show(Live.recv(v1_daemon)))
WebSockex.send_frame(v1_client, {:text, hello.("hello", Base.encode64(<<0::256>>))})
emit.("handshake_invalid_close\t" <> Live.show(Live.recv(v1_client)))

# v1 replacement
v1_daemon2 = Live.connect("serverId=live_v1&role=server")
emit.("v1_replaced\t" <> Live.show(Live.recv(v1_daemon)))
WebSockex.cast(v1_daemon2, :close)

# oversized control frame closes 1009 without a reason
control3 = Live.connect("serverId=live_big&role=server&v=2")
_ = Live.recv(control3)
WebSockex.send_frame(control3, {:text, String.duplicate("x", 65_537)})
emit.("control_oversize\t" <> Live.show(Live.recv(control3)))

File.write!(output, :ets.tab2list(lines) |> Enum.map(fn {_, text} -> [text, "\n"] end))
System.halt(0)
