# Build steps. Run inside the lab: ./lab make <target>
EFI := target/x86_64-unknown-uefi/release/dosh.efi

.PHONY: all efi guests check test test-fast clean
all: efi guests

efi:
	cargo build --release -p dosh --target x86_64-unknown-uefi

guests:
	$(MAKE) -C guests

# Static checks and the host-side unit tests of the DOS services.
check:
	cargo fmt --check
	cargo clippy -p dosh-dos -p dosh-term -p dosh-fat --all-targets -- -D warnings
	cargo clippy -p dosh --release --target x86_64-unknown-uefi -- -D warnings
	cargo test -p dosh-dos -p dosh-term -p dosh-fat

# Everything: static checks, unit tests, and the acceptance suite on every
# execution path this machine offers.
test: all check
	python3 -m pytest tests

clean:
	rm -rf target build
