#!/bin/bash



function finish {
  echo "Cleaning up"
  echo "1234" | su lukas -l -s /bin/bash -c "echo '1234' | sudo -S kill -2 \$(echo '1234' | sudo -S pidof TestCaseLauncher)"
}
trap finish EXIT
BENCHMARK="$1"
echo "Starting $BENCHMARK"
echo "1234" | su lukas -l -s /bin/bash -c "echo '1234' | sudo -S target/debug/TestCaseLauncher -k script -n 10.0.0.0/16 /data-ssd/lukas/dima/nes-benchmarks/${BENCHMARK}/build/vmlauncher-script.yaml"

