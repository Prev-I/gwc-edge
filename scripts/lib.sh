# Shared by scripts/deploy.sh and scripts/seed.sh (sourced, not run).

# The D1 database name declared in wrangler.jsonc.
d1_name() {
  local name
  name="$(grep -o '"database_name": "[^"]*"' wrangler.jsonc | head -1 | cut -d'"' -f4 || true)"
  [ -n "$name" ] || { echo "no database_name in wrangler.jsonc" >&2; return 1; }
  echo "$name"
}

# The id of the remote D1 database named $1, or nothing if there is none.
d1_id() {
  wrangler d1 list --json | node -e '
    let s = ""; process.stdin.on("data", (d) => (s += d)).on("end", () => {
      const db = JSON.parse(s).find((d) => d.name === process.argv[1]);
      if (db) process.stdout.write(db.uuid);
    });' "$1"
}
