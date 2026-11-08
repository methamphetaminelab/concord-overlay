# concord-voice protocol v1

Unix socket: `$XDG_RUNTIME_DIR/concord-voice.sock` (mode 0600), created by patched concord.
Disable export with env `CONCORD_VOICE_EXPORT=0`.

Server → client only. Newline-delimited JSON (UTF-8), one object per line.
Every line is the FULL current voice state; the client simply replaces its state.
A line is sent immediately when a client connects, then whenever the state changes
(speaking start/stop, join/leave, mute/deaf change, channel change). Identical states are not resent.

```json
{"v":1,"self_id":"123","channel":{"id":"456","name":"General","guild_id":"789"},
 "members":[{"id":"123","name":"me","avatar":"https://cdn.discordapp.com/avatars/123/abc.png",
   "speaking":false,"mute":false,"deaf":false,"self_mute":false,"self_deaf":false,"stream":false}]}
```

- `self_id`: string or null (not logged in yet).
- `channel`: null when not in a voice channel (then `members` is `[]`). `guild_id` null for DM/group calls.
- `members`: everyone in the current channel incl. self; order = by id (client sorts as it likes).
- `avatar`: full URL or null (client picks a default).
- IDs are strings (Discord snowflakes exceed JS-safe ints).
- Unknown fields must be ignored by clients; `v` bumps only on breaking change.
- Slow clients are dropped by the server (never block concord). Clients reconnect.
