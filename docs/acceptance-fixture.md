# Real-project acceptance fixture

Scale acceptance runs against a large real corpus that is **not** named in this
repository. Point the tooling at it through local configuration:

```bash
export RCDVLOG_ACCEPTANCE_MEDIA_ROOT="/absolute/path/to/your/large/corpus"
```

Everything below assumes that variable. Never hard-code the path in Git, in an
issue, or in a commit message: it names somebody's private footage, and the
technical value of an acceptance run is in the counts and the behaviour, not in
where the files live.

It is source media, not a disposable test directory. Before accessing it:

- Confirm the volume is mounted and macOS permission has been granted.
- Enumerate read-only first; permission prompts may require the user to click Allow and retry.
- Never write generated files into the source tree.
- Use a project catalog under the configured laptop-local projects root.
- Start with a single source/small subset for smoke testing before any all-media batch.
- Report exact scan/import/failure counts and preserve partial successful work.

Scale acceptance should cover thousands of assets, offline behavior, progress responsiveness, estimated time remaining, horizontal story-ribbon navigation, and restart/interruption behavior. It is periodic manual acceptance, not part of every test run.
