"""Offline ownership decision for the disposable ingress DNS record."""

from dataclasses import dataclass
from ipaddress import IPv4Address
import re


RECORD_ID = re.compile(r"[A-Za-z0-9_-]{1,128}")
DNS_LABEL = re.compile(r"[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?")


@dataclass(frozen=True)
class DnsRecord:
    """Normalized receipt or provider record, scoped to the zone read."""

    zone_id: str
    record_id: str
    name: str
    record_type: str
    content: str
    proxied: bool
    ttl: int
    tags: tuple[str, ...]
    comment: str | None


def valid_record(record: DnsRecord | None) -> bool:
    if not isinstance(record, DnsRecord) or not all(
        isinstance(value, str) for value in (record.zone_id, record.record_id, record.name, record.content)
    ):
        return False
    name = record.name.removesuffix(".")
    if (not RECORD_ID.fullmatch(record.zone_id) or not RECORD_ID.fullmatch(record.record_id)
            or len(name) > 253 or not all(DNS_LABEL.fullmatch(label) for label in name.split("."))
            or record.record_type != "A" or record.proxied is not True
            or type(record.ttl) is not int or record.ttl != 1 or record.tags != () or record.comment is not None):
        return False
    try:
        IPv4Address(record.content)
    except ValueError:
        return False
    return True


def select_owned_record(
    receipt: DnsRecord,
    by_id: DnsRecord | None,
    by_name: tuple[DnsRecord, ...] | None,
) -> tuple[str, str] | None:
    """Return the receipt's (zone ID, record ID) when both reads agree.

    Supply the creation receipt and fresh, independent provider reads by ID
    and exact name. The caller validates and normalizes provider responses,
    including the zone and empty metadata, and supplies the complete name
    result. Use None for unavailable or invalid evidence.

    A None decision leaves cleanup pending, including when both reads are
    empty. This decision does not establish post-delete absence or authorize
    any operation on its own.
    """
    if not valid_record(receipt) or not valid_record(by_id) or not isinstance(by_name, tuple) or len(by_name) != 1:
        return None
    if valid_record(by_name[0]) and by_id == receipt == by_name[0]:
        return receipt.zone_id, receipt.record_id
    return None
