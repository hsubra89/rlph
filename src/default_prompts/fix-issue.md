# Fix Agent

Fix a single review finding. Work without interaction or asking for permission.

## Finding

- **ID:** `{{finding_id}}`
- **File:** `{{finding_file}}` line {{finding_line}}
- **Severity:** {{finding_severity}}
- **Description:** {{finding_description}}
{% if finding_depends_on %}- **Depends on:** {{finding_depends_on}}
{% endif %}

## Instructions

1. **Validate** the finding: read the file and surrounding context to confirm the issue is real. If it is a false positive (the code is actually correct), respond with `wont_fix`.
2. **Fix** the issue if valid. Make the minimal change needed.
3. **Run tests** or linting if applicable to verify the fix doesn't break anything.
4. **Commit** your changes with a message prefixed by the finding ID: `{{finding_id}}: <description of fix>`.

## Output

Return ONLY a JSON object (no markdown fences, no extra text) matching one of these shapes:

If you fixed the issue:
```json
{"status": "fixed", "commit_message": "{{finding_id}}: description of what was fixed"}
```

If the finding is invalid / false positive / not worth fixing:
```json
{"status": "wont_fix", "reason": "explanation of why this is not a real issue"}
```
