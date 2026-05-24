#!/usr/bin/env python3
# Temporary B11-3d diagnostic: extract a file from a Frame OS FS disk image by
# path, so a tcc-produced /out.elf can be analyzed with host objdump/readelf.
# Mirrors shared/src/fs.rs (block 0 superblock, bitmap, inode table, then data;
# inode = kind u16 / nlink u16 / size u32 / direct[28] u32 / indirect u32 /
# double_indirect u32; dirent = name[28] + inode u32).
import sys, struct

BLOCK = 512
INODE_SIZE = 128
INODES_PER_BLOCK = 4
NDIRECT = 28
DIRENT_SIZE = 32
BITMAP_START = 1
INODE_BLOCKS = 16
PTRS = 128  # PTRS_PER_BLOCK
ROOT = 1

def main(img_path, want_path):
    with open(img_path, "rb") as f:
        disk = f.read()
    def block(b):
        return disk[b*BLOCK:(b+1)*BLOCK]
    total = struct.unpack_from("<I", disk, 4)[0]  # superblock.total_blocks
    bitmap_blocks = (total + 4095) // 4096
    inode_start = BITMAP_START + bitmap_blocks
    def read_inode(ino):
        blk = inode_start + ino // INODES_PER_BLOCK
        off = (ino % INODES_PER_BLOCK) * INODE_SIZE
        b = block(blk)
        kind, nlink, size = struct.unpack_from("<HHI", b, off)
        direct = list(struct.unpack_from("<28I", b, off + 8))
        indirect, dind = struct.unpack_from("<II", b, off + 8 + NDIRECT*4)
        return kind, size, direct, indirect, dind
    def block_for(direct, indirect, dind, i):
        if i < NDIRECT:
            return direct[i]
        i -= NDIRECT
        if i < PTRS:
            return struct.unpack_from("<I", block(indirect), i*4)[0]
        i -= PTRS
        l1, l2 = i // PTRS, i % PTRS
        mid = struct.unpack_from("<I", block(dind), l1*4)[0]
        return struct.unpack_from("<I", block(mid), l2*4)[0]
    def read_file(ino):
        kind, size, direct, indirect, dind = read_inode(ino)
        data = b""
        nb = (size + BLOCK - 1) // BLOCK
        for i in range(nb):
            data += block(block_for(direct, indirect, dind, i))
        return data[:size]
    # Resolve path from root.
    ino = ROOT
    for comp in want_path.strip("/").split("/"):
        kind, size, direct, indirect, dind = read_inode(ino)
        found = None
        nb = (size + BLOCK - 1) // BLOCK
        for i in range(nb):
            b = block(block_for(direct, indirect, dind, i))
            for o in range(0, BLOCK, DIRENT_SIZE):
                name = b[o:o+28].split(b"\0")[0].decode("latin1")
                di = struct.unpack_from("<I", b, o+28)[0]
                if di != 0 and name == comp:
                    found = di
                    break
            if found:
                break
        if not found:
            print(f"not found: {comp}", file=sys.stderr); sys.exit(1)
        ino = found
    sys.stdout.buffer.write(read_file(ino))

if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
