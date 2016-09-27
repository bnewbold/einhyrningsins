
RONN =	ronn
MANPAGE =	einhyrningsins.1

$(MANPAGE): doc/$(MANPAGE).ronn
	$(RONN) -r $<

$(MANPAGE).html: doc/$(MANPAGE).ronn
	$(RONN) -5 $<
