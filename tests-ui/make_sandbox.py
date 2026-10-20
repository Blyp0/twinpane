#!/usr/bin/env python3
"""Create the practice folder used by the UI tests (and handy for trying Twinpane safely).
usage: python3 make_sandbox.py /tmp/tp-sandbox      (needs: pip install pillow)"""
import os, shutil, sys
from PIL import Image

root = sys.argv[1] if len(sys.argv) > 1 else "/tmp/tp-sandbox"
shutil.rmtree(root, ignore_errors=True)
os.makedirs(root)
os.chdir(root)
os.makedirs("Photos"); os.makedirs("Documents")
cams = [("Canon", "Canon EOS R5"), ("Nikon", "NIKON Z 6"), ("Sony", "ILCE-7M4")]
dates = ["2023:04:05 17:27:00", "2021:06:01 10:00:00", "2024:12:24 08:15:30",
         "2019:03:03 21:45:10", "2022:08:14 13:05:00", "2023:11:02 09:30:00"]
for i, dt in enumerate(dates, 1):
    img = Image.new("RGB", (64, 48), (40 * i % 255, 90, 160))
    ex = Image.Exif()
    mk, md = cams[i % 3]
    ex[0x010F] = mk
    ex[0x0110] = md
    ex.get_ifd(0x8769)[0x9003] = dt
    img.save(f"Photos/IMG_{i:04d}.jpg", exif=ex)
Image.new("RGB", (32, 32)).save("Photos/no_exif.png")
open("Documents/notes.txt", "w").write("todo: buy milk\nsecond line\n")
open("Documents/Report.TXT", "w").write("quarterly report\nTODO: send\n")
open("Documents/.hidden_note", "w").write("hidden")
os.makedirs("Sync-A/sub"); os.makedirs("Sync-B/sub")
open("Sync-A/only_left.txt", "w").write("L")
open("Sync-B/only_right.txt", "w").write("R")
open("Sync-A/sub/shared.txt", "w").write("new version")
open("Sync-B/sub/shared.txt", "w").write("old")
os.utime("Sync-A/sub/shared.txt", (2_000_000_000, 2_000_000_000))
os.utime("Sync-B/sub/shared.txt", (1_500_000_000, 1_500_000_000))
os.makedirs("Big")
for i in range(3000):
    open(f"Big/file_{i:04d}.dat", "w").write("x" * (i % 50))
print("sandbox ready:", root)
