"""QM Satellite Worker — OS-level process isolation for satellites.

Each SatelliteWorker wraps a Satellite instance in a separate
``multiprocessing.Process``, communicating exclusively via the
shared memory ring buffer.

Architecture:
    Hub Process                     Satellite Process (separate PID)
    ──────────                      ──────────────────────────────────
    Hub.dispatch()                  worker._run_loop()
        → ring.publish()     →      ring.consume()
                                        → satellite._execute_command()
        ← ring.collect()    ←      ring.complete()

Zero shared Python objects — only mmap bytes cross the boundary.
"""

from __future__ import annotations

import logging
import multiprocessing
import os
import signal
import time
from dataclasses import dataclass
from typing import Callable, Optional

from qm_core.ipc.ring_buffer import SharedRingBuffer

logger = logging.getLogger("qm.worker")


@dataclass
class WorkerSpec:
    """Specification for spawning a satellite worker process."""
    satellite_id: str          # "vec-0", "gen-0", "plqm-0"
    satellite_type: str        # "vector", "general", "procedure"
    ring_path: str             # Path to shared memory file
    slot_count: int = 1024
    slot_data_size: int = 65536
    data_dir: str = "/tmp/qm_sat"
    dim: int = 128             # Vector dimension (for vector satellite)
    poll_interval_us: int = 100


def _satellite_main(spec: WorkerSpec) -> None:
    """Entry point for a satellite child process.

    Runs in its own PID, separate memory space.
    Only touches the ring buffer via mmap.
    """
    # Attach to existing ring (consumer side)
    ring = SharedRingBuffer(
        path=spec.ring_path,
        slot_count=spec.slot_count,
        slot_data_size=spec.slot_data_size,
        create=False,  # Attach — don't create
    )

    # Instantiate the satellite (inside the child process)
    from qm_core.satellite.base import SatelliteConfig

    config = SatelliteConfig(
        satellite_id=spec.satellite_id,
        poll_interval_us=spec.poll_interval_us,
        data_dir=spec.data_dir,
    )

    if spec.satellite_type == "vector":
        from qm_core.satellite.vector_satellite import VectorSatellite
        sat = VectorSatellite(config, ring, dim=spec.dim)
    elif spec.satellite_type == "general":
        from qm_core.satellite.general_satellite import GeneralSatellite
        sat = GeneralSatellite(config, ring)
    elif spec.satellite_type == "procedure":
        from qm_core.satellite.procedure_satellite import ProcedureSatellite
        sat = ProcedureSatellite(config, ring)
    else:
        raise ValueError(f"Unknown satellite type: {spec.satellite_type}")

    logger.info(
        "Satellite %s (type=%s, pid=%d) started — attached to ring %s",
        spec.satellite_id, spec.satellite_type, os.getpid(), spec.ring_path,
    )

    # Poll loop — runs until SIGTERM or parent death
    sat.start()
    try:
        while True:
            time.sleep(1.0)
    except (KeyboardInterrupt, SystemExit):
        pass
    finally:
        sat.stop()
        logger.info("Satellite %s (pid=%d) stopped", spec.satellite_id, os.getpid())


class SatelliteWorker:
    """Manages one satellite as an isolated OS process.

    Usage:
        worker = SatelliteWorker(spec)
        worker.spawn()       # fork child process
        worker.is_alive()    # check health
        worker.shutdown()    # graceful stop
    """

    def __init__(self, spec: WorkerSpec) -> None:
        self._spec = spec
        self._process: Optional[multiprocessing.Process] = None

    @property
    def spec(self) -> WorkerSpec:
        return self._spec

    @property
    def pid(self) -> int | None:
        return self._process.pid if self._process else None

    def spawn(self) -> None:
        """Spawn the satellite as a child process."""
        if self._process and self._process.is_alive():
            return

        self._process = multiprocessing.Process(
            target=_satellite_main,
            args=(self._spec,),
            name=f"qm-sat-{self._spec.satellite_id}",
            daemon=True,
        )
        self._process.start()
        logger.info(
            "Spawned satellite %s as pid=%d",
            self._spec.satellite_id, self._process.pid,
        )

    def is_alive(self) -> bool:
        return self._process is not None and self._process.is_alive()

    def shutdown(self, timeout: float = 5.0) -> None:
        """Gracefully shut down the satellite process."""
        if self._process is None:
            return

        if self._process.is_alive():
            self._process.terminate()
            self._process.join(timeout=timeout)
            if self._process.is_alive():
                self._process.kill()
                self._process.join(timeout=1.0)

        logger.info("Satellite %s shut down", self._spec.satellite_id)
        self._process = None

    def restart(self) -> None:
        """Restart the satellite (e.g., after crash)."""
        self.shutdown()
        self.spawn()


class SatelliteCluster:
    """Manages a fleet of satellite worker processes.

    Usage:
        cluster = SatelliteCluster()
        cluster.add("vec-0", "vector", ring_path="/tmp/ring.shm")
        cluster.add("gen-0", "general", ring_path="/tmp/ring.shm")
        cluster.start_all()
        ...
        cluster.stop_all()
    """

    def __init__(self) -> None:
        self._workers: dict[str, SatelliteWorker] = {}

    def add(self, spec: WorkerSpec) -> None:
        """Register a satellite worker spec."""
        self._workers[spec.satellite_id] = SatelliteWorker(spec)

    def start_all(self) -> None:
        """Spawn all registered satellite processes."""
        for worker in self._workers.values():
            worker.spawn()

    def stop_all(self) -> None:
        """Shut down all satellite processes."""
        for worker in self._workers.values():
            worker.shutdown()

    def is_healthy(self) -> bool:
        """Check if all satellites are alive."""
        return all(w.is_alive() for w in self._workers.values())

    def get_worker(self, satellite_id: str) -> SatelliteWorker | None:
        return self._workers.get(satellite_id)

    def restart_dead(self) -> list[str]:
        """Restart any dead satellites. Returns list of restarted IDs."""
        restarted = []
        for sid, worker in self._workers.items():
            if not worker.is_alive():
                worker.restart()
                restarted.append(sid)
        return restarted

    @property
    def workers(self) -> dict[str, SatelliteWorker]:
        return dict(self._workers)
