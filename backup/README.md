# Stock firmware backup

Full 8 MB flash dump of the Tidbyt Gen 1 (MAC b4:8a:0a:4a:00:a4), taken 2026-09-19
before any custom firmware was written. The `.bin` is git-ignored; keep a copy
somewhere safe.

    tidbyt-stock-b48a0a4a00a4.bin  8388608 bytes
    sha256 36a416dd0bf3b1d243dcb2c59e4419e391f026a7454d7ec2c98476dfe7c3a287

Partition table (at 0x8000): nvs 0x9000+0x5000, otadata 0xe000+0x2000,
app0 0x10000+0x3f0000, app1 0x400000+0x3f0000.

Since card 210 (2026-09-20) the device carries screeny's own table instead
(`firmware/partitions.csv`: nvs, otadata, ota_0 and ota_1 at 2 MB each, a 64 KB
`screeny` settings partition). Restoring the stock image below overwrites all of it.

Restore (baud must stay <= 230400 on this bench; your own backup will have a
different name and hash):

    esptool --port $SCREENY_PORT --baud 230400 write-flash 0 tidbyt-stock-b48a0a4a00a4.bin

Taken with `tools/backup-flash.sh`, which reads in 256 KB chunks and retries,
because long reads corrupt at 460800 baud and above.
