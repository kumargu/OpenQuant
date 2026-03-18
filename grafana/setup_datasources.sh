#!/bin/bash
# Generate Grafana datasource provisioning YAML from journal/*.db files
# Run this before `docker compose up` to pick up new journals

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
JOURNAL_DIR="$SCRIPT_DIR/../data/journal"
OUTPUT="$SCRIPT_DIR/provisioning/datasources/sqlite.yml"

cat > "$OUTPUT" <<'HEADER'
apiVersion: 1
datasources:
HEADER

first=true
for db in "$JOURNAL_DIR"/*.db; do
  [ -f "$db" ] || continue
  name=$(basename "$db" .db)
  path="/var/lib/grafana/journal/$(basename "$db")"

  is_default="false"
  if [ "$first" = true ]; then
    is_default="true"
    first=false
  fi

  cat >> "$OUTPUT" <<EOF
  - name: $name
    type: frser-sqlite-datasource
    access: proxy
    isDefault: $is_default
    editable: true
    jsonData:
      path: $path
EOF
done

echo "Generated datasources for $(ls "$JOURNAL_DIR"/*.db 2>/dev/null | wc -l | tr -d ' ') journal DBs -> $OUTPUT"
