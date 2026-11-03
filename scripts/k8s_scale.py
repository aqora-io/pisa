#!/usr/bin/env python3
"""Checks KEDA autoscaling and graceful drain of a pisa Helm release.

    kubectl port-forward svc/pisa-router 8000:8000 &
    python3 scripts/k8s_scale.py

1. Sends a burst of slow checks and waits for the worker Deployment to scale
   above its minimum, then for every check to pass.
2. Waits for the Deployment to scale back down to its minimum.
3. Deletes the worker pod while it runs a check; the pod drains, so the check
   still passes.
Throughout, the router must not have retried any job: retries would mean a
worker was killed mid-job instead of draining.
"""

import argparse
import json
import os
import shlex
import subprocess
import sys
import threading
import time
import urllib.request

BASE = os.environ.get("PISA_URL", "http://localhost:8000")


def kubectl(args, *cmd):
    out = subprocess.run(shlex.split(args.kubectl) + list(cmd), check=True, capture_output=True)
    return out.stdout.decode().strip()


def replicas(args):
    return int(kubectl(args, "get", "deploy", args.deployment, "-o", "jsonpath={.status.replicas}") or 0)


def slow_check(seconds):
    body = {"code": f"example : True := by\n  sleep {seconds * 1000}\n  trivial", "timeout": seconds + 60}
    req = urllib.request.Request(
        BASE + "/check", json.dumps(body).encode(), {"content-type": "application/json"}
    )
    with urllib.request.urlopen(req, timeout=900) as r:
        return json.loads(r.read())


def retries():
    with urllib.request.urlopen(BASE + "/metrics") as r:
        for line in r.read().decode().splitlines():
            if line.startswith("pisa_job_retries_total"):
                return float(line.split()[-1])
    return 0.0


def run_burst(n, seconds):
    results = [None] * n

    def go(i):
        try:
            results[i] = slow_check(seconds)
        except Exception as e:  # report, don't hang the burst
            results[i] = {"ok": False, "error": str(e)}

    threads = [threading.Thread(target=go, args=(i,)) for i in range(n)]
    for t in threads:
        t.start()
    return threads, results


def wait_for(what, cond, timeout, interval=2):
    deadline = time.time() + timeout
    while time.time() < deadline:
        if cond():
            return
        time.sleep(interval)
    sys.exit(f"timed out waiting for {what}")


def main():
    p = argparse.ArgumentParser()
    # Name the context: `minikube kubectl` otherwise uses the current one.
    p.add_argument("--kubectl", default="minikube -p pisa kubectl -- --context pisa")
    p.add_argument("--deployment", default="pisa-worker")
    p.add_argument("--min", type=int, default=1, help="the ScaledObject's minReplicaCount")
    p.add_argument("--jobs", type=int, default=24)
    p.add_argument("--seconds", type=int, default=20, help="how long each check runs")
    p.add_argument("--scale-down-timeout", type=int, default=300)
    args = p.parse_args()

    wait_for("minimum replicas", lambda: replicas(args) == args.min, args.scale_down_timeout)
    before = retries()

    threads, results = run_burst(args.jobs, args.seconds)
    peak = args.min
    while any(t.is_alive() for t in threads):
        peak = max(peak, replicas(args))
        time.sleep(2)
    failed = [r for r in results if not r.get("ok")]
    assert not failed, failed[:3]
    assert peak > args.min, f"never scaled above {args.min}"
    print(f"ok  {args.jobs} checks passed; workers scaled {args.min} -> {peak}")

    wait_for("scale-down", lambda: replicas(args) == args.min, args.scale_down_timeout)
    print(f"ok  scaled back down to {args.min}")

    pods = kubectl(args, "get", "pods", "-l", "app.kubernetes.io/component=worker", "-o", "name").split()
    threads, results = run_burst(1, args.seconds)
    time.sleep(5)
    # Pods removed by the scale-down may still be listed and exit before the delete.
    kubectl(args, "delete", "--wait=false", "--ignore-not-found", *pods)
    threads[0].join()
    assert results[0].get("ok"), results[0]
    print("ok  check finished on a worker that was deleted mid-job (drained)")

    assert retries() == before, "jobs were retried: a worker was killed mid-job"
    print("ok  no jobs retried")


if __name__ == "__main__":
    main()
