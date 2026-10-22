# Lantern Relay Design

Lantern routing uses a durable queue to retain courier messages until acknowledged.

## Retry Budget

The backoff scheduler retries a failed message three times.

See the [queue decision](../decisions/0001-queue.md).
