SHELL := /bin/bash
PATH := /opt/homebrew/opt/rustup/bin:$(HOME)/.cargo/bin:$(PATH)
export PATH
CARGO := $(shell command -v cargo 2>/dev/null)
ifeq ($(CARGO),)
ifneq ($(wildcard /opt/homebrew/opt/rustup/bin/cargo),)
CARGO := /opt/homebrew/opt/rustup/bin/cargo
else
CARGO := $(HOME)/.cargo/bin/cargo
endif
endif

PATTERN ?= burst
REQUESTS ?= 200
DURATION ?= 10
RATE ?= 20
CONCURRENCY ?= 32

.PHONY: setup dev-up dev-down test test-unit test-integration test-all simulate

setup:
	./scripts/setup.sh

dev-up:
	docker compose up -d --build

dev-down:
	docker compose down --remove-orphans

test: test-unit

test-unit:
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --all-targets --all-features -- -D warnings
	$(CARGO) test
	docker compose config --quiet

test-integration:
	./scripts/integration-test.sh

test-all: test-unit test-integration

simulate:
	./scripts/run-simulation.sh \
		--pattern "$(PATTERN)" \
		--requests "$(REQUESTS)" \
		--duration "$(DURATION)" \
		--rate "$(RATE)" \
		--concurrency "$(CONCURRENCY)"
