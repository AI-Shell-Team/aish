"""
Cancellation token system for graceful interruption of nested operations.

This module provides a hierarchical cancellation mechanism that allows
parent operations to cancel child operations cleanly.
"""

import threading
from enum import Enum
from typing import Callable, Optional

import anyio


class CancellationReason(Enum):
    """Reasons for cancellation"""

    USER_INTERRUPT = "user_interrupt"
    TIMEOUT = "timeout"
    PARENT_CANCELLED = "parent_cancelled"
    ERROR = "error"


class CancellationToken:
    """
    Thread-safe cancellation token that can be shared across async and sync contexts.

    Provides both blocking and non-blocking cancellation checking for different
    execution contexts (async/sync, threads, etc.).
    """

    def __init__(self, parent: Optional["CancellationToken"] = None):
        """
        Initialize cancellation token.

        Args:
            parent: Optional parent token. If parent is cancelled, this token
                   will also be considered cancelled.
        """
        self._cancelled = threading.Event()
        self._reason: Optional[CancellationReason] = None
        self._message: Optional[str] = None
        self._parent = parent
        self._callbacks: list[Callable[[], None]] = []
        self._lock = threading.Lock()
        # Track AnyIO cancel scopes associated with this token
        self._scopes: set[anyio.CancelScope] = set()

    def cancel(
        self,
        reason: CancellationReason = CancellationReason.USER_INTERRUPT,
        message: Optional[str] = None,
    ):
        """
        Cancel the token with a specific reason.

        Args:
            reason: The reason for cancellation
            message: Optional additional message
        """
        with self._lock:
            if self._cancelled.is_set():
                return  # Already cancelled

            self._reason = reason
            self._message = message
            self._cancelled.set()

            # Async waiters will poll via is_cancelled(); no need to notify an event here

            # Cancel any active AnyIO cancel scopes
            try:
                # Copy set to avoid modification during iteration
                scopes_snapshot = list(self._scopes)
                for scope in scopes_snapshot:
                    try:
                        scope.cancel()
                    except Exception:
                        # Ignore individual scope cancel errors
                        pass
            except Exception:
                # Be resilient to unexpected errors while cancelling scopes
                pass

            # Call registered callbacks
            for callback in self._callbacks:
                try:
                    callback()
                except Exception:
                    # Ignore callback errors to prevent cancellation failure
                    pass

    def is_cancelled(self) -> bool:
        """Check if cancellation has been requested (non-blocking)."""
        # Check self first
        if self._cancelled.is_set():
            return True

        # Check parent if exists
        if self._parent and self._parent.is_cancelled():
            # Propagate parent cancellation
            if not self._cancelled.is_set():
                self.cancel(
                    CancellationReason.PARENT_CANCELLED,
                    "Cancelled due to parent operation cancellation",
                )
            return True

        return False

    # Note: Explicit cancellation checking removed - rely on AnyIO CancelScope instead
    # Use open_cancel_scope() to bind semantic context to structural cancellation

    def check_cancelled_sync(self):
        """
        Check for cancellation in sync context and raise if cancelled.

        Raises:
            KeyboardInterrupt: If cancellation has been requested
        """
        if self.is_cancelled():
            reason = self.get_cancellation_reason()
            message = self.get_cancellation_message()
            raise KeyboardInterrupt(
                f"Operation cancelled: {reason.value}"
                + (f" - {message}" if message else "")
            )

    def get_cancellation_reason(self) -> Optional[CancellationReason]:
        """Get the reason for cancellation."""
        return self._reason

    def get_cancellation_message(self) -> Optional[str]:
        """Get the cancellation message."""
        return self._message

    def add_cancellation_callback(self, callback: Callable[[], None]):
        """
        Add a callback to be called when cancellation occurs.

        Args:
            callback: Function to call on cancellation (should not raise)
        """
        with self._lock:
            self._callbacks.append(callback)

            # If already cancelled, call immediately
            if self._cancelled.is_set():
                try:
                    callback()
                except Exception:
                    pass

    def create_child_token(self) -> "CancellationToken":
        """Create a child cancellation token that inherits from this one."""
        return CancellationToken(parent=self)

    # ----- AnyIO integration helpers -----
    def _register_scope(self, scope: anyio.CancelScope):
        with self._lock:
            self._scopes.add(scope)

    def _unregister_scope(self, scope: anyio.CancelScope):
        with self._lock:
            self._scopes.discard(scope)

    class _TokenScope:
        """
        Synchronous context manager that enters an AnyIO CancelScope and
        registers it to this CancellationToken for coordinated cancellation.
        """

        def __init__(self, token: "CancellationToken", shield: bool = False):
            self._token = token
            self._scope = anyio.CancelScope(shield=shield)

        def __enter__(self) -> anyio.CancelScope:
            self._token._register_scope(self._scope)
            return self._scope.__enter__()

        def __exit__(self, exc_type, exc, tb):
            try:
                return self._scope.__exit__(exc_type, exc, tb)
            finally:
                self._token._unregister_scope(self._scope)

    def open_cancel_scope(
        self, shield: bool = False
    ) -> "CancellationToken._TokenScope":
        """
        Create a CancelScope tied to this token.

        Usage (inside async code):
            with token.open_cancel_scope():
                await do_work()

        When this token is cancelled, the scope will be cancelled as well,
        causing a cancellation at the next checkpoint inside the scope.
        """
        return CancellationToken._TokenScope(self, shield=shield)

    # Note: Async waiting removed - use AnyIO primitives (sleep_forever, move_on_after) directly

    def wait_for_cancellation_sync(self, timeout: Optional[float] = None):
        """
        Wait for cancellation to be requested (blocking).

        Args:
            timeout: Maximum time to wait in seconds

        Returns:
            bool: True if cancelled, False if timeout
        """
        while not self.is_cancelled():
            if self._cancelled.wait(timeout=0.1):
                return True
            if timeout is not None:
                timeout -= 0.1
                if timeout <= 0:
                    return False
        return True
