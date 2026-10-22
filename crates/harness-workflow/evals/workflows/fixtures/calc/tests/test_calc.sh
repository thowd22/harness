. ./src/calc.sh
. ./tests/lib.sh
check add "$(add 2 3)" 5
check sub "$(sub 5 3)" 2
finish
