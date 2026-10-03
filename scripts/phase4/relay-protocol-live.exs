# Drives the running pinned relay over real sockets and prints the raw wire it answers with.
# Usage (inside the baseline project, test env): mix run relay-protocol-live.exs OUT
# Masked: ts (wall_clock) and the generated connection id (generated_id).

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

  def recv(client, timeout \\ 1_500) do
    receive do
      {:frame, ^client, kind, payload} -> {kind, payload}
      {:closed, ^client, {:remote, code, reason}} -> {:close, code, reason}
      {:closed, ^client, other} -> {:closed, inspect(other)}
    after
      timeout -> :none
    end
  end

  def drain(client, label, emit) do
    case recv(client, 300) do
      :none -> :ok
      other ->
        emit.("#{label}\t" <> show(other))
        drain(client, label, emit)
    end
  end

  def show({kind, payload}) when kind in [:text, :binary], do: "#{kind} " <> mask(payload)
  def show({:close, code, reason}), do: "close #{code} #{inspect(reason)}"
  def show(other), do: inspect(other)

  defp mask(payload) do
    payload
    |> String.replace(~r/"ts":\d+/, ~s("ts":<wall_clock>))
    |> String.replace(~r/conn_[0-9a-f]{16}/, "<generated_id>")
  end

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

[output] = System.argv()
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
emit.("generated_connected\t" <> Live.show(Live.recv(control)))

control2 = Live.connect("serverId=live_a&role=server&v=2")
emit.("control_replaced_old\t" <> Live.show(Live.recv(control)))
emit.("control_replaced_new_sync\t" <> Live.show(Live.recv(control2)) |> String.replace(~r/"conn_[^"]*"/, ~s("<generated_id>")))

WebSockex.cast(client, :close)
Process.sleep(200)
emit.("client_left_control\t" <> Live.show(Live.recv(control2)))
emit.("client_left_data\t" <> Live.show(Live.recv(data)))
WebSockex.cast(generated, :close)
Process.sleep(200)
Live.drain(control2, "control_late", emit)

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

# sync order beyond a small map
big = Live.connect("serverId=live_many&role=server&v=2")
_ = Live.recv(big)
clients =
  for i <- 1..40 do
    c = Live.connect("serverId=live_many&role=client&v=2&connectionId=conn_#{i}")
    _ = Live.recv(big)
    c
  end
big2 = Live.connect("serverId=live_many&role=server&v=2")
emit.("sync_40\t" <> Live.show(Live.recv(big2)))
Enum.each(clients, &WebSockex.cast(&1, :close))

# handshake validation closes the client with 1008
{public_key, _} = :crypto.generate_key(:ecdh, :x25519)
hello = fn type, key -> Jason.encode!(%{type: type, key: key, capabilities: %{}}) end
v1_daemon = Live.connect("serverId=live_v1&role=server")
v1_client = Live.connect("serverId=live_v1&role=client")
WebSockex.send_frame(v1_client, {:text, hello.("hello", Base.encode64(public_key))})
emit.("handshake_valid_forwarded\t" <> (Live.show(Live.recv(v1_daemon)) |> String.replace(Base.encode64(public_key), "<key>")))
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
