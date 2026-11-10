# Immutable vertical-motion original-production golden

Concatenate part-00.json-fragment through part-06.json-fragment in that exact
order, with no separators. These are UTF-8-safe slices of the existing golden,
not regenerated expectations or fewer samples. The reconstructed bytes must
have SHA256 4012a7af5a2820cbbfb26f25cd1d9668b2c190fbfb90615d4cb9e1baeaca68b9
and size 28,821,908 bytes. The test checks both before decoding the same intern
format. manifest.json records every part and the unchanged original checksum.

The original capture provenance remains in ../vertical_motion_provenance.json.
The unsharded original is retained in the independently sealed recovery baseline.
Sharding keeps each normal GitHub blob request below the transport size limit.
