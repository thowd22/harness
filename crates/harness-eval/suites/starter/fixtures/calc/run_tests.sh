# Runs every test; exits non-zero on the first failing file.
for t in tests/test_*.sh; do
  sh "$t" || { echo "FAILED: $t"; exit 1; }
done
echo "all tests passed"
