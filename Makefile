
RONN =	ronn
MANPAGE =	einhyrningsins.1

doc/$(MANPAGE): doc/$(MANPAGE).ronn
	$(RONN) -r $<

doc/$(MANPAGE).html: doc/$(MANPAGE).ronn
	$(RONN) --style toc -5 $<

gh-pages: doc/$(MANPAGE).html
	cp doc/$(MANPAGE).html /tmp/index.html
	git checkout gh-pages
	cp /tmp/index.html index.html
	git add -u index.html
	git commit -m "updating rendered manpage for github docs"
	git checkout master
