
RONN =	ronn
MANPAGE =	einhyrningsins.1

docs: doc/*.ronn
	$(RONN) -r doc/*.ronn
	$(RONN) --style toc -5 doc/*.ronn

gh-pages: docs
	mkdir -p /tmp/einhyrningsins-ronn
	cp doc/*.1.html /tmp/einhyrningsins-ronn
	git checkout gh-pages
	cp /tmp/einhyrningsins-ronn/*.html .
	git add -u *.html
	git commit -m "updating rendered manpage for github docs" || true
	git checkout master
	rm -r /tmp/einhyrningsins-ronn
