#!/usr/bin/env bash
# Copy the repository rulesets from another repo onto this one.
#
# Rulesets need a public repository or GitHub Pro, so this cannot run while
# hitui is private. Once it is public:
#
#     gh repo edit guttermonk/hitui --visibility public --accept-visibility-change-consequences
#     .github/apply-rulesets.sh
#
# Reads live from the source repo rather than from a saved copy, so it stays
# correct if the source rulesets change.
set -euo pipefail

FROM="${1:-guttermonk/slappyshot}"
TO="${2:-guttermonk/hitui}"

echo "Copying rulesets from $FROM to $TO"

gh api "repos/$FROM/rulesets" --jq '.[].id' | while read -r id; do
    # Keep only the fields the create endpoint accepts, and drop null
    # parameters — the API rejects an explicit null where it expects an object.
    gh api "repos/$FROM/rulesets/$id" --jq '{
        name,
        target,
        enforcement,
        conditions,
        rules: [.rules[] | {type} + (if .parameters then {parameters: .parameters} else {} end)]
    }' > /tmp/ruleset-$id.json

    name=$(jq -r .name "/tmp/ruleset-$id.json")
    if gh api "repos/$TO/rulesets" --method POST --input "/tmp/ruleset-$id.json" --silent; then
        echo "  created: $name"
    else
        echo "  FAILED:  $name" >&2
    fi
    rm -f "/tmp/ruleset-$id.json"
done

echo
echo "Rulesets now on $TO:"
gh api "repos/$TO/rulesets" --jq '.[] | "  \(.name) [\(.target)] \(.enforcement)"'
