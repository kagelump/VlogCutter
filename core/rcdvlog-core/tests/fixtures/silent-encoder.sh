#!/bin/sh
# Signal readiness from the running child, not from the test process. The last
# argument is the staged output path; writing beside it keeps the repository
# fixture immutable so no concurrent fork can hold it open for writing.
for last_argument in "$@"; do
    :
done
: > "${last_argument}.started"
exec sleep 30
