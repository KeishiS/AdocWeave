#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 3 ] || [ -z "$1" ]; then
  echo "usage: dependabot-alert-snapshot.sh OWNER/REPOSITORY DEPENDENCIES_JSON CHANGED_FILES_JSON" >&2
  exit 2
fi

repository="$1"
dependencies="$2"
changed_files="$3"
snapshot_directory="$(mktemp -d)"
trap 'rm -rf "$snapshot_directory"' EXIT

jq -e '
  type == "array"
  and all(.[]; type == "string" and length > 0)
' <<< "$dependencies" > /dev/null
jq -e '
  type == "array"
  and length > 0
  and all(.[]; type == "string" and length > 0)
' <<< "$changed_files" > /dev/null

for state in open fixed dismissed auto_dismissed; do
  gh api --paginate \
    "repos/$repository/dependabot/alerts?state=$state&per_page=100" \
    | jq -es --arg expected_state "$state" '
        if length > 0 and all(.[]; type == "array")
        then
          add
          | if all(
              .[];
              type == "object"
              and .state == $expected_state
              and (.dependency | type) == "object"
              and (.dependency.manifest_path | type) == "string"
              and (.dependency.manifest_path | length) > 0
              and (.dependency.package | type) == "object"
              and (.dependency.package.name | type) == "string"
              and (.dependency.package.name | length) > 0
            )
            then .
            else error("Dependabot alert entries must match the requested state and schema")
            end
        else error("Dependabot alert response must contain one or more arrays")
        end
      ' > "$snapshot_directory/$state.json"
done

jq -n \
  --argjson dependencies "$dependencies" \
  --argjson changed_files "$changed_files" \
  --slurpfile open "$snapshot_directory/open.json" \
  --slurpfile fixed "$snapshot_directory/fixed.json" \
  --slurpfile dismissed "$snapshot_directory/dismissed.json" \
  --slurpfile auto_dismissed "$snapshot_directory/auto_dismissed.json" \
  '
    def normalized_path:
      sub("^\\./"; "") | sub("^/+"; "");
    ($changed_files | map(normalized_path)) as $paths |
    ([$open[0][], $fixed[0][], $dismissed[0][], $auto_dismissed[0][]]) as $alerts |
    {
      lookupCompleted: true,
      openCount: ($open[0] | length),
      securityUpdate: any(
        $alerts[];
        . as $alert |
        ($alert.dependency?.manifest_path? | type) == "string"
        and (
          $paths
          | index($alert.dependency.manifest_path | normalized_path)
        ) != null
        and (
          ($dependencies | length) == 0
          or (
            ($alert.dependency?.package?.name? | type) == "string"
            and ($dependencies | index($alert.dependency.package.name)) != null
          )
        )
      ),
      stateCounts: {
        open: ($open[0] | length),
        fixed: ($fixed[0] | length),
        dismissed: ($dismissed[0] | length),
        autoDismissed: ($auto_dismissed[0] | length)
      }
    }
  '
