def non_empty_string:
  type == "string" and length > 0;

def nullable_string:
  . == null or type == "string";

def valid_auto_merge:
  type == "object"
  and (.enabled_by.login | non_empty_string)
  and (
    .merge_method == "merge"
    or .merge_method == "squash"
    or .merge_method == "rebase"
  )
  and (.commit_title | nullable_string)
  and (.commit_message | nullable_string);

def valid_pull_request:
  type == "object"
  and has("auto_merge")
  and (.number | type == "number" and . > 0 and floor == .)
  and .number == $pr_number
  and (.node_id | non_empty_string)
  and .user.login == "dependabot[bot]"
  and .base.ref == "main"
  and .base.repo.full_name == $repository
  and .head.repo.full_name == $repository
  and (.head.ref | type == "string" and startswith("dependabot/"))
  and (.auto_merge == null or (.auto_merge | valid_auto_merge));

if valid_pull_request
then {
  nodeId: .node_id,
  autoMergeEnabled: (.auto_merge != null)
}
else error("Pull Request detail response is invalid or its identity changed")
end
