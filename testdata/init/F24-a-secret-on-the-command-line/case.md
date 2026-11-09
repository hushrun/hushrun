# F24: a secret on the command line

**Pattern.** `pytest --token ${{ secrets.NPM_TOKEN }}`, and `pytest --token $TOKEN` where the job's
`env` sets `TOKEN` from a secret; beside them `pytest --level $LEVEL`, whose variable nothing in the
file sets.

**Mirrors.** Projects whose tests reach a registry or an API with a token. A negative case.

**Expected after Phase 4.** Each line is left, with why: a secret on the command line would be in
the result's record, and a variable set nowhere in the file may hold one. CI's file isn't edited.
