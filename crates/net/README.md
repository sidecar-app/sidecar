# net

WebRTC peer connections and signaling client for Sidecar.

## Integration tests

The integration tests exercise the signaling client against a live server. Set `SIGNALING_URL` to the WebSocket endpoint of a compatible signaling server, then run:

```sh
SIGNALING_URL=ws://localhost:8080 cargo test -p net -- --include-ignored
```
