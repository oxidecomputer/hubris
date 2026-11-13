# Host integration tests

robo: Each test image here runs one real task on the host kernel, surrounded
robo: by fakes for the tasks it calls, and checks its behavior scenario by
robo: scenario. Virtual time fast-forwards while every task is blocked, so
robo: multi-second timeouts cost nothing.

## Running

robo: `cargo xtask host-test host-test/psc-seq/app.toml` runs every scenario;
robo: append scenario names to run a subset. `--show-output` prints the
robo: output of passing scenarios too.

## Layout

robo: - `api`: control protocol, typed clients for the fakes, and orchestrator
robo:   helpers (`wait_until`, `check!`, `fail`).
robo: - `supervisor`: task 0. Any task fault fails the run.
robo: - `fake-sys`, `fake-i2c`, `fake-packrat`: stand-ins for hardware-facing
robo:   tasks. Each serves the real API plus one control operation.
robo: - `psc-seq`: image and orchestrator for `drv-psc-seq-server`.

## How a run works

robo: `xtask host-test` builds the image once, then runs the kernel once per
robo: scenario with `HUBRIS_HOST_TEST_SCENARIO` set. The task named
robo: `orchestrator` reads it, drives the fakes, and checks results. The run
robo: passes if the orchestrator asks the supervisor for a reset; it fails if
robo: any task faults or halts, or if `--stop-at` is reached.

## Writing fakes

robo: Fakes model one device or service and nothing more: register files,
robo: pin levels, logs. Behavior that couples devices (a PSU's `PWR_OK`
robo: dropping while it is held off) belongs in the scenario. Topology
robo: (EXTI wiring, I2C devices) comes from the image's manifest through the
robo: same codegen the real tasks use.

## Adding an image

robo: Copy `psc-seq`: a manifest with `target = "host"`, the task under test,
robo: the fakes it needs, and an `orchestrator` task whose `scenarios` config
robo: lists the scenario names its `main` dispatches on.
