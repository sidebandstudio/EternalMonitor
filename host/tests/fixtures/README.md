# Test fixtures

`vdd_settings-25.5.2.xml` is the default `vdd_settings.xml` from
[Virtual-Display-Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver)
25.5.2 (`Virtual Display Driver (HDR)/vdd_settings.xml`), byte for byte. It is
MIT licensed by its authors. `host/src/vdd_settings.rs` tests that the host
edits this file in place.

`vdd_settings-25.5.2-cp949.xml` and `vdd_settings-25.5.2-cp936.xml` are that
file with a comment holding a folder path and a card name in non-ASCII text,
saved in Windows code page 949 and 936 as Notepad's ANSI does: the
`encoding='utf-8'` declaration stays. They were made with Python's `cp949` and
`gbk` codecs, so they do not depend on the Windows conversion under test.
