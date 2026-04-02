"""Process that handles SIGUSR1 for checkpoint and runs indefinitely."""
import os
import signal
import sys
import time

STATE_FILE = os.environ.get("STATE_FILE", "/tmp/ckpt")
counter = 0


def handle_checkpoint(signum, frame):
    global counter
    with open(STATE_FILE, "w") as f:
        f.write(f"checkpoint counter={counter}\n")
    print(f"CHECKPOINT saved={STATE_FILE} counter={counter}", flush=True)


def handle_term(signum, frame):
    print("TERMINATED", flush=True)
    sys.exit(0)


signal.signal(signal.SIGUSR1, handle_checkpoint)
signal.signal(signal.SIGTERM, handle_term)

print(f"RUNNING state_file={STATE_FILE}", flush=True)
while True:
    counter += 1
    time.sleep(1)
