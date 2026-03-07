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
      "suggested_fixes": ["<fix-1>", "<fix-2>"],
      "category": "<category>",
      "depends_on": ["<other-finding-id>"] | null
    }
  ]
}
```

- `id`: short slugified identifier (lowercase, hyphens, max 50 chars).
- `file`: use the current path in the repository. For renamed files use the new path.
- `line`: use a 1-based line number on the new/current side of the diff. Do not use deleted-line numbers or `0`.
- `description`: use backticks around code references (function names, variable names, type names, file paths) for readability.
- `suggested_fixes`: at least one concrete, actionable fix the author can apply. Each entry is a short description of a distinct fix option.
- `depends_on`: array of finding `id`s this finding is blocked by, or `null`.
- Every finding must be actionable — do not report information-only observations. If you cannot propose a concrete fix, do not report it.
- Consolidate closely related issues into a single finding instead of splitting them into multiple small findings. Use the description to cover all related aspects.
- Return an empty `findings` array when there are no issues.
