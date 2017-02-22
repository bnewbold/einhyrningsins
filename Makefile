
RONN =	ronn
MANPAGE =	einhyrningsins.1

INSTALL = install
PREFIX = /usr/local

.PHONY: docs
docs: doc/*.ronn
	$(RONN) -r doc/*.ronn
	$(RONN) --style toc -5 doc/*.ronn

.PHONY: gh-pages
gh-pages: docs
	mkdir -p /tmp/einhyrningsins-ronn
	cp doc/*.1.html /tmp/einhyrningsins-ronn
	git checkout gh-pages
	cp /tmp/einhyrningsins-ronn/*.html .
	git add -u *.html
	git commit -m "updating rendered manpage for github docs" || true
	git checkout master
	rm -r /tmp/einhyrningsins-ronn

.PHONY: build
build: src/*.rs src/bin/*.rs
	cargo build --release

.PHONY: install
install:
	$(INSTALL) -t $(PREFIX)/bin target/release/einhyrningsins
	$(INSTALL) -t $(PREFIX)/bin target/release/einhyrningsinsctl
	# Trying to install manpages; ok if this fails
	$(INSTALL) -m 644 -t $(PREFIX)/share/man/man1 doc/einhyrningsins.1
	$(INSTALL) -m 644 -t $(PREFIX)/share/man/man1 doc/einhyrningsinsctl.1
