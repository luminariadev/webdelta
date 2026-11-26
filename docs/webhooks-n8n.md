# Webhooks and n8n Integration

When a watch transitions and matches its configured conditions, `webdelta` posts a structured JSON payload to every webhook notifier listed on that watch.

## Payload Schema

```json
{
  "event": "watch.changed",
  "watch_id": "laptop-price",
  "watch_name": "Workstation Laptop Price",
  "url": "https://shop.example.com/products/workstation-pro",
  "checked_at": "2026-11-26T12:00:00Z",
  "previous_value": "$1,299.00",
  "current_value": "$1,099.00",
  "numeric_delta": {
    "previous": 1299.0,
    "current": 1099.0,
    "diff": -200.0,
    "percent_change": -15.396458814472672
  },
  "diff_summary": "+1 / -1 lines",
  "diff": "--- previous\n+++ current\n@@ -1 +1 @@\n-$1,299.00\n+$1,099.00\n",
  "matched_rules": [
    "dropped by 15.40% (threshold 10.00%)"
  ]
}
```

Note that `numeric_delta` is `null` if either the previous or current extracted text does not contain a parseable number.

## Verifying HMAC Signatures

If `secret` is configured on a `webhook` notifier, `webdelta` computes an HMAC-SHA256 digest over the raw UTF-8 request body bytes and attaches:

```text
X-WebDelta-Signature: sha256=<lowercase hex digest>
```

Example verification in Node.js (for an n8n Code node or custom backend):

```javascript
const crypto = require("crypto");

const rawBody = JSON.stringify($json.body);
const expected =
  "sha256=" +
  crypto
    .createHmac("sha256", process.env.WEBDELTA_WEBHOOK_SECRET)
    .update(rawBody)
    .digest("hex");

if ($json.headers["x-webdelta-signature"] !== expected) {
  throw new Error("Invalid webdelta webhook signature");
}
```

For production verification against raw unparsed HTTP bytes, verify the exact byte payload received by your ingress proxy or middleware before JSON re-serialization.

## Importing the Example n8n Workflow

An importable workflow template is included at [n8n-workflow.json](../examples/n8n-workflow.json). It listens for `webdelta` POST payloads, checks whether `numeric_delta.percent_change <= -10`, and routes matching alerts to Telegram.
