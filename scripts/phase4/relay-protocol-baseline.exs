# Runs the pinned relay's own wire functions over a corpus and prints one raw line per case.
# Usage (inside the baseline project): mix run --no-start relay-protocol-baseline.exs IN OUT
# Input lines are tab separated: kind, then arguments (Base64 unless stated).

defmodule RelayProtocolBaseline do
  def b64(binary), do: Base.encode64(binary)
  def dec(text), do: Base.decode64!(text)

  def run("hs", [payload]) do
    case PaseoRelay.HandshakeValidation.check(:text, dec(payload)) do
      :not_handshake -> "not_handshake"
      {:accept, type} -> "accept #{type}"
      {:reject, type} -> "reject #{type}"
    end
  end

  def run("ping", [payload]) do
    with {:ok, %{"type" => "ping"}} <- Jason.decode(dec(payload)) do
      "ping"
    else
      _ -> "ignore"
    end
  end

  def run("qs", [query]) do
    case parse_qs(dec(query)) do
      :error ->
        "qs_error"

      {:ok, pairs} ->
        shown =
          Enum.map_join(pairs, ",", fn
            {name, true} -> b64(name) <> ":flag"
            {name, value} -> b64(name) <> ":" <> b64(value)
          end)

        map =
          Map.new(pairs, fn
            {name, true} -> {name, ""}
            {name, value} -> {name, value}
          end)

        "pairs=" <> shown <> " " <> connection(map)
    end
  end

  def run("sync", [ids]) do
    keys = ids |> parse_ids() |> Map.new(&{&1, true}) |> Map.keys()
    encode(%{type: "sync", connectionIds: keys})
  end

  # A map that grows past 32 keys and then loses some: the relay's disconnect path.
  def run("syncdel", [ids, deleted]) do
    map = ids |> parse_ids() |> Map.new(&{&1, true})
    shrunk = Enum.reduce(parse_ids(deleted), map, fn id, acc -> Map.delete(acc, id) end)
    encode(%{type: "sync", connectionIds: Map.keys(shrunk)})
  end

  def run("connected", [id]), do: encode(%{type: "connected", connectionId: dec(id)})
  def run("disconnected", [id]), do: encode(%{type: "disconnected", connectionId: dec(id)})
  def run("pong", [ts]), do: encode(%{type: "pong", ts: String.to_integer(ts)})

  def run("limits", []) do
    [
      PaseoRelay.Protocol.maximum_frame_wire_bytes(),
      PaseoRelay.Protocol.maximum_client_frame_payload_bytes(),
      PaseoRelay.Protocol.maximum_message_payload_bytes(),
      PaseoRelay.Protocol.maximum_control_payload_bytes()
    ]
    |> Enum.join(" ")
  end

  # Only the Cowlib call may fail: a crash in Connection.from_query must stay visible.
  defp parse_qs(query) do
    {:ok, :cow_qs.parse_qs(query)}
  rescue
    _ -> :error
  end

  defp parse_ids("none"), do: []
  defp parse_ids(ids), do: ids |> String.split(",") |> Enum.map(&parse_id/1)
  defp parse_id("_"), do: ""
  defp parse_id(id), do: dec(id)

  defp encode(map) do
    "ok " <> Jason.encode!(map)
  rescue
    _ -> "error"
  end

  defp connection(map) do
    case PaseoRelay.Connection.from_query(map) do
      {:ok, connection} ->
        id =
          case connection.connection_id do
            nil -> "nil"
            value -> connection_id(connection, map, value)
          end

        "ok role=#{connection.role} serverId=#{b64(connection.server_id)} v=#{connection.version} connectionId=#{id}"

      {:error, message} ->
        "error #{message}"
    end
  end

  defp connection_id(connection, map, value) do
    if connection.role == :client and connection.version == 2 and
         String.trim(Map.get(map, "connectionId") || "") == "" do
      if Regex.match?(~r/\Aconn_[0-9a-f]{16}\z/, value), do: "generated", else: "BAD_GENERATED"
    else
      b64(value)
    end
  end
end

[input, output] = System.argv()

lines =
  input
  |> File.stream!()
  |> Stream.map(&String.trim_trailing(&1, "\n"))
  |> Enum.map(fn line ->
    [kind | arguments] = String.split(line, "\t")
    line <> "\t=>\t" <> RelayProtocolBaseline.run(kind, arguments)
  end)

File.write!(output, Enum.map(lines, &[&1, "\n"]))
