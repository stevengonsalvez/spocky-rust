#!/bin/sh
# Stands in for Claude Code on the stream-json protocol. It answers the SDK's
# control requests, and after the first user message plays back the frames
# of $FAKE_FRAMES (one JSON frame per line; `#flush` ends a chunk, `#sleep S`
# waits, `#wait-response` reads one line from stdin, `#stderr TEXT` writes to
# stderr, `#exit N` exits). Every line it reads is appended to $FAKE_LOG.
request_id() { printf '%s' "$1" | sed -n 's/.*"request_id":"\([^"]*\)".*/\1/p'; }
chunk=""
flush() {
  if [ -n "$chunk" ]; then printf '%s' "$chunk"; chunk=""; fi
}
play() {
  while IFS= read -r frame <&3; do
    case "$frame" in
      "#flush") flush;;
      "#sleep "*) flush; sleep "${frame#\#sleep }";;
      "#wait-response") flush; IFS= read -r reply; printf '%s\n' "$reply" >> "$FAKE_LOG";;
      "#stderr "*) printf '%s\n' "${frame#\#stderr }" >&2;;
      "#exit "*) flush; exit "${frame#\#exit }";;
      *) chunk="$chunk$frame
";;
    esac
  done
  flush
}
: > "$FAKE_LOG"
exec 3< "$FAKE_FRAMES"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$FAKE_LOG"
  case "$line" in
    *'"type":"control_request"'*)
      id=$(request_id "$line")
      case "$line" in
        *'"subtype":"initialize"'*)
          printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{"commands":[],"agents":[],"output_style":"default","available_output_styles":["default"],"models":[],"account":{}}}}\n' "$id";;
        *)
          printf '{"type":"control_response","response":{"subtype":"success","request_id":"%s","response":{}}}\n' "$id";;
      esac;;
    *'"type":"user"'*) play;;
  esac
done
