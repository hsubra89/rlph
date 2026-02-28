Respond with a single JSON object (no markdown fences, no commentary outside the JSON). The schema:

```json
{
  "findings": [
    {
      "id": "<short-slugified-id>",
      "file": "<path>",
      "line": <number>,
      "severity": "critical" | "warning" | "info",
      "description": "<description>",
      "category": "<category>",
      "depends_on": ["<other-finding-id>"] | null
    }
  ]
}
```

- `id`: short slugified identifier (lowercase, hyphens, max 50 chars).
- `depends_on`: array of finding `id`s this finding is blocked by, or `null`.
- Return an empty `findings` array when there are no issues.
