`encrypted-rar4.rar` is the 185-byte `test_read_format_rar_encryption_data.rar`
fixture from the [libarchive test suite](https://github.com/libarchive/libarchive/blob/master/libarchive/test/test_read_format_rar_encryption_data.rar.uu).
Its password is `12345678`. The libarchive fixture is distributed under the
libarchive BSD license; see the [test source](https://github.com/libarchive/libarchive/blob/master/libarchive/test/test_read_format_rar_encryption_data.c).

The four `test_read_format_rar5_multiarchive_solid.partNN.rar` files are from
the [libarchive multi-volume fixture](https://github.com/libarchive/libarchive/tree/master/libarchive/test).
They use the same libarchive BSD license and verify extraction when all parts
are in one directory.

`encrypted-header-rar4.rar` is the 220-byte
[`test_read_format_rar_encryption_header.rar`](https://github.com/libarchive/libarchive/blob/master/libarchive/test/test_read_format_rar_encryption_header.rar.uu)
fixture from libarchive. Its password is `12345678`; the
[test source](https://github.com/libarchive/libarchive/blob/master/libarchive/test/test_read_format_rar_encryption_header.c)
documents its encrypted headers and BSD license.

`depotkit_abi.c` is an offline test engine that checks the public depotkit request
fields, progress callback, and cancellation control without making network requests.
