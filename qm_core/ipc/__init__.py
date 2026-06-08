"""QM IPC — Shared Memory Data Plane.

LMAX Disruptor-style lock-free ring buffer for zero-copy
inter-process communication between Hub and Satellite processes.

Includes a Slab Allocator for large media blobs that cannot fit
through ring-buffer slots (images, video, audio).
"""

from qm_core.ipc.ring_buffer import (
    SharedRingBuffer,
    SlotState,
    SlotHeader,
    SLOT_HEADER_SIZE,
)
from qm_core.ipc.media_allocator import (
    MediaSlabAllocator,
    MediaAllocatorConfig,
    SlabHandle,
)

__all__ = [
    "SharedRingBuffer",
    "SlotState",
    "SlotHeader",
    "SLOT_HEADER_SIZE",
    "MediaSlabAllocator",
    "MediaAllocatorConfig",
    "SlabHandle",
]
