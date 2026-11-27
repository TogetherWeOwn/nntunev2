"""Qualify the private SeaweedFS lifecycle using independent admin requests."""

from hashlib import sha256
import json
import secrets
from urllib.parse import quote, unquote, urlencode
import xml.etree.ElementTree as ET

from contract import KEY, PATH, bulk_body, bulk_checksum, denied_matrix, require, require_denial
from qualify import wait_for
from render import BUCKET
from wire import request_bytes, response, signed


ENDPOINT = "127.0.0.1:8333"
BUCKET_PATH = f"/{BUCKET}"
USER = "synthetic-fixture"
POLICY = {
    "Version": "2012-10-17",
    "Statement": [
        {"Effect": "Allow", "Action": ["s3:GetObjectVersion", "s3:DeleteObjectVersion"],
         "Resource": f"arn:aws:s3:::{BUCKET}/{KEY}"},
        {"Effect": "Deny", "Action": ["s3:ListAllMyBuckets", "s3:ListBucket*", "s3:GetObject",
                                      "s3:DeleteObject", "s3:PutObject*"], "Resource": "*"},
    ],
}


def version_target(version):
    return f"{PATH}?versionId={quote(version, safe='')}"


def xml(root, name):
    return root.findtext(f".//{{*}}{name}")


def inventory_field(row, name):
    fields = row.findall(f"{{*}}{name}")
    require(len(fields) == 1 and not len(fields[0]) and fields[0].text, "invalid inventory field")
    return fields[0].text


def expect_status(reply, status, label):
    require(reply[0] == status, f"{label} returned {reply[0]}")
    return reply[2]


def present(reply, version, label):
    status, headers, body = reply
    require((status, headers.get("x-amz-version-id"), body) == (200, [version], b""), label)


def absent(reply, label):
    status, _, body = reply
    require((status, body) == (404, b""), label)


def deleted(reply, version, label):
    status, headers, body = reply
    require((status, body) == (204, b"") and headers.get("x-amz-version-id", [version]) == [version], label)


class Storage:
    def __init__(self, stack):
        self.stack = stack

    def request(self, method, target, fields, body):
        wire = self.stack.backend_wire(request_bytes(method, target, fields, body))
        return response(wire, decode_body=method not in ("HEAD", "DELETE"))

    def iam_reply(self, action, **fields):
        body = urlencode({"Action": action, "Version": "2010-05-08", **fields}).encode()
        headers = signed("POST", "/", ENDPOINT, self.stack.admin, body,
                         (("content-type", "application/x-www-form-urlencoded"),), service="iam")
        return self.request("POST", "/", headers, body)

    def iam(self, action, **fields):
        return ET.fromstring(expect_status(self.iam_reply(action, **fields), 200, action))

    def s3(self, method, target, body=b"", *, identity=None, extra=()):
        fields = signed(method, target, ENDPOINT, self.stack.admin if identity is None else identity, body, extra)
        return self.request(method, target, fields, body)

    def upload(self, body):
        reply = self.s3("PUT", PATH, body)
        expect_status(reply, 200, "fixture upload")
        versions = reply[1].get("x-amz-version-id", [])
        require(len(versions) == 1 and versions[0] not in ("", "null"), "upload did not return a version")
        return versions[0]

    def provision(self):
        def ready():
            # During startup the port may not yet accept a connection.
            try:
                return self.s3("GET", "/")[0] == 200
            except RuntimeError:
                return False
        wait_for(ready, "private admin endpoint did not start")
        expect_status(self.s3("PUT", BUCKET_PATH), 200, "create bucket")
        versioning = b"<VersioningConfiguration><Status>Enabled</Status></VersioningConfiguration>"
        lifecycle = (b"<LifecycleConfiguration><Rule><ID>fixture-backstop</ID><Filter><Prefix></Prefix></Filter>"
                     b"<Status>Enabled</Status><NoncurrentVersionExpiration><NoncurrentDays>1</NoncurrentDays>"
                     b"</NoncurrentVersionExpiration></Rule></LifecycleConfiguration>")
        expect_status(self.s3("PUT", BUCKET_PATH + "?versioning", versioning), 200, "enable versioning")
        expect_status(self.s3("PUT", BUCKET_PATH + "?lifecycle", lifecycle), 200, "configure expiry")
        for query, field, expected in (("versioning", "Status", "Enabled"), ("lifecycle", "NoncurrentDays", "1")):
            body = expect_status(self.s3("GET", f"{BUCKET_PATH}?{query}"), 200, "bucket configuration readback")
            require(xml(ET.fromstring(body), field) == expected, "bucket configuration mismatch")

        self.iam("CreateUser", UserName=USER)
        created = self.iam("CreatePolicy", PolicyName=USER, PolicyDocument=json.dumps(POLICY))
        self.arn = xml(created, "Arn")
        self.iam("AttachUserPolicy", UserName=USER, PolicyArn=self.arn)
        key = self.iam("CreateAccessKey", UserName=USER)
        identity = xml(key, "AccessKeyId"), xml(key, "SecretAccessKey")
        attached = self.iam("ListAttachedUserPolicies", UserName=USER)
        arns = [row.text for row in attached.findall(".//{*}PolicyArn")]
        require(arns == [self.arn], "managed policy attachment mismatch")
        version = xml(self.iam("GetPolicy", PolicyArn=self.arn), "DefaultVersionId")
        doc = xml(self.iam("GetPolicyVersion", PolicyArn=self.arn, VersionId=version), "Document")
        require(doc is not None and json.loads(unquote(doc)) == POLICY, "managed policy readback mismatch")
        require(all(identity), "scoped credential missing")
        self.selected = self.upload(b"fixture selected")
        self.sibling = self.upload(b"fixture sibling")
        require(self.selected != self.sibling, "uploads returned the same version")
        return identity

    def inventory(self):
        result = ET.fromstring(expect_status(self.s3("GET", BUCKET_PATH + "?versions"), 200, "version inventory"))
        uploads = ET.fromstring(expect_status(self.s3("GET", BUCKET_PATH + "?uploads"), 200, "multipart inventory"))
        for root, expected in ((result, "ListVersionsResult"), (uploads, "ListMultipartUploadsResult")):
            require(root.tag.rsplit("}", 1)[-1] == expected, "unexpected inventory response")
            require(inventory_field(root, "IsTruncated") == "false", "inventory incomplete")
        return (
            sorted((inventory_field(item, "Key"), inventory_field(item, "VersionId"), inventory_field(item, "ETag"))
                   for item in result.findall("{*}Version")),
            sorted((inventory_field(item, "Key"), inventory_field(item, "VersionId"))
                   for item in result.findall("{*}DeleteMarker")),
            sorted(inventory_field(item, "UploadId") for item in uploads.findall("{*}Upload")),
        )

    def audit_barrier(self):
        marker = "fixture-audit-" + secrets.token_hex(8)
        present(self.s3("HEAD", version_target(self.sibling), extra=(("user-agent", marker),)),
                self.sibling, "audit sentinel HEAD")

        def read():
            raw = self.stack.run("exec", "-T", "backend", "cat", "/tmp/nntune-audit").lstrip()
            decoder = json.JSONDecoder()
            entries = []
            while raw:
                try:
                    row, end = decoder.raw_decode(raw)
                except json.JSONDecodeError:
                    return None  # Last append may still be in flight.
                require(row[0].endswith("s3.access"), "unexpected storage audit event")
                entries.append(row[2])
                raw = raw[end:].lstrip()
            return entries if entries and entries[-1].get("user_agent") == marker else None

        return wait_for(read, "storage audit did not record the sentinel")

    def negative_control(self, boundary):
        control = self.upload(b"fresh bulk control")
        body = bulk_body(control)
        checksum = bulk_checksum(body)
        probes = (
            ("list-all", self.s3("GET", "/", identity=boundary.credential), "ListAllMyBucketsResult"),
            ("bulk delete", self.s3("POST", BUCKET_PATH + "?delete", body, identity=boundary.credential,
                                    extra=(("content-md5", checksum),)), "DeleteResult"),
        )
        for label, reply, expected_root in probes:
            payload = ET.fromstring(expect_status(reply, 200, "direct " + label))
            require(payload.tag.split("}")[-1] == expected_root, "negative control had wrong response")
            try:
                require_denial(reply, label)
            except RuntimeError:
                continue
            raise RuntimeError("negative control passed deny assertion")
        item = ET.fromstring(probes[1][1][2]).find(".//{*}Deleted")
        require(item is not None and (xml(item, "Key"), xml(item, "VersionId")) == (KEY, control),
                "direct bulk control did not report exact deletion")
        absent(self.s3("HEAD", version_target(control)), "direct bulk control did not delete its version")
        print("Bypass control reproduced list-all and exact bulk deletion. Both failed the denial assertion.", flush=True)

    def dispose(self, boundary):
        access, _ = boundary.credential
        self.iam("DeleteAccessKey", UserName=USER, AccessKeyId=access)
        keys = self.iam("ListAccessKeys", UserName=USER)
        require(xml(keys, "AccessKeyId") is None, "revoked access key remains")
        self.iam("DetachUserPolicy", UserName=USER, PolicyArn=self.arn)
        self.iam("DeleteUser", UserName=USER)
        self.iam("DeletePolicy", PolicyArn=self.arn)
        for action, fields in (("GetUser", {"UserName": USER}), ("GetPolicy", {"PolicyArn": self.arn})):
            status, _, body = self.iam_reply(action, **fields)
            require(status == 404 and xml(ET.fromstring(body), "Code") == "NoSuchEntity",
                    "disposable IAM resource remains")
        status, _, body = self.s3("GET", "/", identity=boundary.credential)
        require(status == 403 and xml(ET.fromstring(body), "Code") == "InvalidAccessKeyId",
                "revoked key did not return InvalidAccessKeyId")
        deleted(self.s3("DELETE", version_target(self.sibling)), self.sibling, "sibling disposal")
        require(self.inventory() == ([], [], []), "disposable versions, markers or uploads remain")
        expect_status(self.s3("DELETE", BUCKET_PATH), 204, "bucket disposal")

    def qualify(self, boundary, bundle):
        mounted = self.stack.run("exec", "-T", "boundary", "cat", "/etc/caddy/Caddyfile").encode()
        digest = sha256(self.stack.config.read_bytes()).hexdigest()
        require(sha256(mounted).hexdigest() == digest, "rendered policy digest mismatch")
        print(f"Read back managed policy and mounted Caddy configuration SHA-256 {digest}.", flush=True)
        before = self.inventory()
        require([(key, version) for key, version, _ in before[0]] ==
                sorted((KEY, version) for version in (self.selected, self.sibling))
                and before[1:] == ([], []), "initial version inventory mismatch")
        events = self.audit_barrier()
        denied_matrix(boundary, self.selected)
        closing = self.audit_barrier()
        require(closing[:-1] == events, "denied requests reached the SeaweedFS audit log")
        require(self.inventory() == before, "denied requests changed backend inventory")
        print("Denied requests produced zero SeaweedFS audit entries and left inventory unchanged.", flush=True)

        access, secret = boundary.credential
        status, headers, body = boundary.request("HEAD", version_target(self.selected),
                                                 identity=(access, secret + "bad"))
        require((status, body) == (403, b"") and "x-nntune-boundary" not in headers,
                "backend did not enforce SigV4 independently")
        for version in (self.selected, self.sibling):
            present(self.s3("HEAD", version_target(version)), version, "private present version HEAD")
            present(boundary.request("HEAD", version_target(version)), version, "present version HEAD")
        if bundle:
            from launcher import qualified_launcher
            qualified_launcher(self.stack, bundle, boundary, self.selected, self.sibling)
        else:
            for _ in range(2):
                deleted(boundary.request("DELETE", version_target(self.selected)), self.selected, "scoped DELETE")
                absent(boundary.request("HEAD", version_target(self.selected)), "scoped absent HEAD")
        absent(self.s3("HEAD", version_target(self.selected)), "independent admin absent HEAD")
        present(boundary.request("HEAD", version_target(self.sibling)), self.sibling, "preserved scoped sibling")
        present(self.s3("HEAD", version_target(self.sibling)), self.sibling, "preserved admin sibling")
        remaining = [row for row in before[0] if row[1] == self.sibling]
        require(self.inventory() == (remaining, [], []), "independent exact-version inventory mismatch")
        print("Confirmed exact-version absence and unchanged sibling with independent admin reads.", flush=True)
        self.negative_control(boundary)
        self.dispose(boundary)
        print("Revoked the disposable identity and verified the backend inventory was empty.", flush=True)
