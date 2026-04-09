from __future__ import annotations

from collections.abc import Callable

from .models import InteractionRequest, InteractionResponse, InteractionStatus


class InteractionService:
    def __init__(
        self,
        renderer: Callable[[InteractionRequest], InteractionResponse],
    ) -> None:
        self._renderer = renderer

    def request(self, request: InteractionRequest) -> InteractionResponse:
        try:
            return self._renderer(request)
        except KeyboardInterrupt:
            raise
        except Exception as exc:
            return InteractionResponse(
                interaction_id=request.id,
                status=InteractionStatus.UNAVAILABLE,
                reason="error",
                metadata={"exception_type": type(exc).__name__},
            )