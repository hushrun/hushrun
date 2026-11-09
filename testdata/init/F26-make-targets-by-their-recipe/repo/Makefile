# The targets CI runs, by the names Node's Makefile gives its own.
PYTEST ?= pytest
PYTEST_ARGS ?=
IMAGE ?= ghcr.io/acme/shop

.PHONY: test-ci lint-ci test-deploy image

test-ci: | lint-ci
	@echo "Running the tests with $(PYTEST_ARGS)..."
	$(PYTEST) -q $(PYTEST_ARGS)

lint-ci:
	ruff check .

# Runs the tests, then ships what passed.
test-deploy: test-ci image
	docker push $(IMAGE):latest

image:
	docker build -t $(IMAGE):latest .
