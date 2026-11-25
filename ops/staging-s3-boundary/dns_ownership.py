"""Offline ownership decision for the disposable ingress DNS record."""

from dataclasses import dataclass


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
    if not (receipt.zone_id and receipt.record_id and receipt.name):
        return None
    if by_id == receipt and by_name == (receipt,):
        return receipt.zone_id, receipt.record_id
    return None
