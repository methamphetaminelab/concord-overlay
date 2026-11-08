#!/usr/bin/env python3
"""Fake concord voice socket for testing concord-overlay without Discord.

Creates the socket (default $XDG_RUNTIME_DIR/concord-voice.sock) and plays a
scenario: join a channel with 4 people, speaking on/off, mute, deaf, a quiet
stretch long enough to see the fade, someone leaves, channel null.
Every line is the full state (protocol/PROTOCOL.md); new clients get the
current state immediately.

  fake-concord.py [--socket PATH] [--loop] [--hold STEP]

--hold STEP stops the scenario after step number STEP (see the printed list)
and keeps serving that state until Ctrl+C.
"""
import argparse
import copy
import json
import os
import select
import signal
import socket
import sys
import time

CDN = "https://cdn.discordapp.com/embed/avatars/{}.png"
ME = {"id": "284112637946462209", "name": "kudurayv", "avatar": CDN.format(0)}
ALICE = {"id": "190425783946362880", "name": "Alice", "avatar": CDN.format(3)}
BOB = {"id": "330786514902827009", "name": "bob", "avatar": CDN.format(4)}
CAROL = {"id": "512043339462885376", "name": "Кэрол", "avatar": None}
CHANNEL = {"id": "880000000000000001", "name": "General", "guild_id": "870000000000000001"}


def member(m, **flags):
    out = dict(m, speaking=False, mute=False, deaf=False, self_mute=False, self_deaf=False, stream=False)
    out.update(flags)
    return out


class Voice:
    def __init__(self):
        self.channel = None
        self.members = {}

    def line(self):
        members = sorted(self.members.values(), key=lambda m: m["id"]) if self.channel else []
        state = {"v": 1, "self_id": ME["id"], "channel": self.channel, "members": members}
        return (json.dumps(state, ensure_ascii=False) + "\n").encode()


def join(v):
    v.channel = CHANNEL
    v.members = {m["id"]: member(m) for m in (ME, ALICE, BOB, CAROL)}


def setf(who, **flags):
    def step(v):
        v.members[who["id"]].update(flags)
    return step


def both(*steps):
    def step(v):
        for s in steps:
            s(v)
    return step


def leave(who):
    def step(v):
        v.members.pop(who["id"], None)
    return step


def channel_null(v):
    v.channel = None
    v.members = {}


SCENARIO = [
    (0.5, "connected, not in a voice channel", lambda v: None),
    (1.5, "join General: kudurayv, Alice, bob, Кэрол (no avatar)", join),
    (2.0, "Alice speaks", setf(ALICE, speaking=True)),
    (2.0, "Alice stops; bob and kudurayv speak", both(setf(ALICE, speaking=False), setf(BOB, speaking=True), setf(ME, speaking=True))),
    (4.0, "everyone quiet", both(setf(BOB, speaking=False), setf(ME, speaking=False))),
    (1.5, "Кэрол mutes herself", setf(CAROL, self_mute=True)),
    (1.5, "bob deafens himself", setf(BOB, self_deaf=True)),
    (1.5, "Alice speaks while Кэрол is muted", setf(ALICE, speaking=True)),
    (2.0, "quiet for 18 s (fade starts after 10 s)", setf(ALICE, speaking=False)),
    (18.0, "Alice speaks (back to full opacity)", setf(ALICE, speaking=True)),
    (2.0, "Alice stops; bob leaves", both(setf(ALICE, speaking=False), leave(BOB))),
    (3.0, "channel null (left the call)", channel_null),
    (3.0, "end of scenario", lambda v: None),
]


def main():
    runtime = os.environ.get("XDG_RUNTIME_DIR", "/tmp")
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--socket", default=os.path.join(runtime, "concord-voice.sock"))
    ap.add_argument("--loop", action="store_true", help="repeat the scenario forever")
    ap.add_argument("--hold", type=int, metavar="STEP", help="stop after this step and keep its state")
    args = ap.parse_args()

    if os.path.exists(args.socket):
        probe = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            probe.connect(args.socket)
            sys.exit(f"fake-concord: {args.socket} is in use (real concord running?)")
        except OSError:
            os.unlink(args.socket)
        finally:
            probe.close()

    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    old_umask = os.umask(0o177)
    server.bind(args.socket)
    os.umask(old_umask)
    server.listen(8)
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
    print(f"fake-concord: serving {args.socket}", flush=True)

    voice, clients = Voice(), []

    def send(conn, data):
        try:
            conn.sendall(data)
            return True
        except OSError:
            conn.close()
            return False

    def pump(timeout):
        readable, _, _ = select.select([server, *clients], [], [], timeout)
        for s in readable:
            if s is server:
                conn, _ = server.accept()
                if send(conn, voice.line()):
                    clients.append(conn)
                continue
            try:
                gone = not s.recv(4096)
            except OSError:
                gone = True
            if gone:
                s.close()
                clients.remove(s)

    try:
        while True:
            for i, (delay, desc, change) in enumerate(SCENARIO):
                deadline = time.monotonic() + delay
                while (left := deadline - time.monotonic()) > 0:
                    pump(left)
                before = voice.line()
                change(voice)
                print(f"fake-concord: [{i}] {desc}", flush=True)
                if voice.line() != before:
                    clients[:] = [c for c in clients if send(c, voice.line())]
                if args.hold is not None and i == args.hold:
                    print("fake-concord: holding this state, Ctrl+C to stop", flush=True)
                    while True:
                        pump(None)
            if not args.loop:
                break
    except KeyboardInterrupt:
        pass
    finally:
        server.close()
        try:
            os.unlink(args.socket)
        except FileNotFoundError:
            pass


if __name__ == "__main__":
    main()
