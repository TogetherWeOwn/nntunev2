"""Verify executable inputs already staged in Linux supervisor custody."""

import errno
import hashlib
import os
import re
import stat

PATH_FLAGS = os.O_PATH | os.O_CLOEXEC | os.O_NOFOLLOW


def _check_owner(info: os.stat_result, owner: int) -> None:
    if info.st_uid != owner or info.st_mode & (stat.S_IWGRP | stat.S_IWOTH):
        raise ValueError("executable path is not supervisor-owned and write-protected")


def _open_executable_at(root_fd: int, parts: list[str], owner: int) -> int:
    directory = os.dup(root_fd)
    try:
        for part in parts[:-1]:
            _check_owner(os.fstat(directory), owner)
            child = os.open(part, PATH_FLAGS | os.O_DIRECTORY, dir_fd=directory)
            os.close(directory)
            directory = child
        _check_owner(os.fstat(directory), owner)
        return os.open(parts[-1], PATH_FLAGS, dir_fd=directory)
    finally:
        os.close(directory)


def _verify_at(root_fd: int, parts: list[str], owner: int, expected_sha256: str) -> int:
    path_fd = _open_executable_at(root_fd, parts, owner)
    try:
        info = os.fstat(path_fd)
        _check_owner(info, owner)
        if (not stat.S_ISREG(info.st_mode) or info.st_nlink != 1
                or not info.st_mode & stat.S_IXOTH
                or info.st_mode & (stat.S_ISUID | stat.S_ISGID)):
            raise ValueError("expected a regular unprivileged executable with no hard links")
        # Reopen only after fstat rejects devices, FIFOs, directories and symlinks.
        # This procfs path refers to the held inode, not the original filename.
        fd = os.open(f"/proc/self/fd/{path_fd}", os.O_RDONLY | os.O_CLOEXEC)
    finally:
        os.close(path_fd)
    try:
        try:
            os.getxattr(fd, "security.capability")
        except OSError as exc:
            if exc.errno != errno.ENODATA:
                raise
        else:
            raise ValueError("executable grants file capabilities")
        with os.fdopen(fd, "rb", closefd=False) as executable:
            digest = hashlib.file_digest(executable, "sha256").hexdigest()
        if digest != expected_sha256:
            raise ValueError("executable digest does not match reviewed input")
        os.lseek(fd, 0, os.SEEK_SET)
        return fd
    except BaseException:
        os.close(fd)
        raise


def open_verified_executable(path: str, expected_sha256: str) -> int:
    """Return a non-inheritable read-only descriptor; caller must close it.

    The supervisor must supply a reviewed digest and a staged path, with a
    trusted mount table and no existing writers to that inode. This checks
    the path and bytes, not the interpreter, dynamic libraries, or process
    that will actually run it. It grants no recipe or candidate admission.
    """
    parts = path.split("/")
    if not path.startswith("/") or any(part in ("", ".", "..") for part in parts[1:]):
        raise ValueError("expected an absolute executable path without aliases")
    if not re.fullmatch(r"[a-f0-9]{64}", expected_sha256):
        raise ValueError("expected a reviewed SHA-256 digest")
    root_fd = os.open("/", PATH_FLAGS | os.O_DIRECTORY)
    try:
        return _verify_at(root_fd, parts[1:], 0, expected_sha256)
    finally:
        os.close(root_fd)
