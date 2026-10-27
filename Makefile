.PHONY: conformance conformance.check hooks audit
lint:
	cargo clippy --all-features -- -D clippy::pedantic -D clippy::perf -D clippy::complexity -D clippy::style -D clippy::correctness -D clippy::suspicious -D clippy::unwrap_used -D clippy::expect_used -D clippy::indexing_slicing
conformance:
	conformance/run.sh
	python3 conformance/report.py readme
conformance.check:
	conformance/run.sh
	python3 conformance/report.py readme --check
hooks:
	git config core.hooksPath .githooks
audit:
	cargo audit
lint.fix:
	cargo clippy --fix --allow-dirty --allow-staged
test:
	cargo test --all-features --locked
build.release:
	cargo build --release
build.dev:
	cargo build
dev:
	cargo watch -c -x run -d 1 -i data -i dist
schema:
	PROKSI_WRITE_SCHEMA=1 cargo test -p super_proxy --bin super config::schema::tests::test_the_published_schema_is_up_to_date
schema.check:
	PROKSI_CHECK_SCHEMA=1 cargo test -p super_proxy --bin super config::schema::tests::test_the_published_schema_is_up_to_date
