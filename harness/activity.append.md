## Session activity (Agent of Empires)

This session reports what it is currently doing as one word, and that word travels in the
session's request headers. Keep it true as the work moves between phases: run
`"${AOE_LLM_TAGS_BIN:-aoe-llm-tags}" activity <word>` when a phase starts — not on every tool
call — where word is one of explore, plan, code, test, debug, review, docs, ops, idle.
`activity --show` prints the current word and the list; `activity --clear` removes it. The
`aoe-activity` skill has the details and the mapping from situations to words.
