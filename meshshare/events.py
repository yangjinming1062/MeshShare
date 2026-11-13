from __future__ import annotations

import queue
import threading
from dataclasses import dataclass
from typing import Any


@dataclass(frozen=True)
class Event:
    revision: int
    resource_type: str
    resource_id: str
    action: str


class EventHub:
    """In-process best-effort fan-out for SSE clients."""

    def __init__(self) -> None:
        self._subscribers: set[queue.Queue[Event]] = set()
        self._lock = threading.Lock()

    def subscribe(self) -> queue.Queue[Event]:
        subscriber: queue.Queue[Event] = queue.Queue(maxsize=128)
        with self._lock:
            self._subscribers.add(subscriber)
        return subscriber

    def unsubscribe(self, subscriber: queue.Queue[Event]) -> None:
        with self._lock:
            self._subscribers.discard(subscriber)

    def publish(self, event: Event) -> None:
        with self._lock:
            subscribers = tuple(self._subscribers)
        for subscriber in subscribers:
            try:
                subscriber.put_nowait(event)
            except queue.Full:
                # A slow client must not block writers. Its next request obtains a full snapshot.
                try:
                    subscriber.get_nowait()
                    subscriber.put_nowait(event)
                except queue.Empty:
                    pass
