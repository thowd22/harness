fail=0
check() { if [ "$2" != "$3" ]; then echo "FAIL $1: expected $3, got $2"; fail=1; fi; }
finish() { exit $fail; }
