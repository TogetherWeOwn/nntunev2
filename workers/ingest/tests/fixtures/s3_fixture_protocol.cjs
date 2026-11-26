'use strict';

const assert = require('node:assert/strict');
const { createHash, createHmac } = require('node:crypto');

const bucket = 'fixture-bucket';
const mediaKey = 'media/abababababab40ab80ababababababab/cdcdcdcdcdcd40cd80cdcdcdcdcdcdcd';
const mapId = 'efefefefefef40ef80efefefefefefef';
const sidecarKey = `media-digests/${mediaKey.slice('media/'.length)}/${mapId}/00000000`;
const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');
const checksum = (bytes) => createHash('sha256').update(bytes).digest('base64');
const mac = (key, text) => createHmac('sha256', key).update(text).digest();
const uploadQuery = 'uploadId=fixture%2B%2F%3D%3F';
const initiatedId = 'new+/=?';
const initiatedQuery = 'uploadId=new%2B%2F%3D%3F';
const completionMarker = 'x-amz-meta-nntune-completion';
const initiationHeaders = {
  'x-amz-checksum-algorithm': 'SHA256',
  'x-amz-checksum-type': 'COMPOSITE',
  [completionMarker]: sha256('nntune-s3-completion\0fixture-lease'),
};

function initiationReceipt(observedBucket = bucket, observedKey = mediaKey, uploadId = initiatedId) {
  return '<?xml version="1.0" encoding="UTF-8"?>'
    + '<InitiateMultipartUploadResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">'
    + `<Bucket>${observedBucket}</Bucket><Key>${observedKey}</Key><UploadId>${uploadId}</UploadId>`
    + '</InitiateMultipartUploadResult>';
}

function completionReceipt(manifest, etag = 'fixture-etag') {
  const prefix = '<CompleteMultipartUpload xmlns="http://s3.amazonaws.com/doc/2006-03-01/">'
    + `<Part><PartNumber>1</PartNumber><ETag>&quot;${etag}&quot;</ETag><ChecksumSHA256>`;
  const suffix = '</ChecksumSHA256></Part></CompleteMultipartUpload>';
  assert.ok(manifest.startsWith(prefix));
  assert.ok(manifest.endsWith(suffix));
  const part = manifest.slice(prefix.length, -suffix.length);
  assert.match(part, /^[A-Za-z0-9+/]{43}=$/);
  const composite = checksum(Buffer.from(part, 'base64'));
  return `<CompleteMultipartUploadResult><Bucket>${bucket}</Bucket><Key>${mediaKey}</Key>`
    + `<ChecksumSHA256>${composite}-1</ChecksumSHA256></CompleteMultipartUploadResult>`;
}

function authorization(method, path, query, fields, access, secret, region) {
  const names = Object.keys(fields).sort();
  const signed = names.join(';');
  const payload = fields['x-amz-content-sha256'];
  const date = fields['x-amz-date'];
  const canonical = [method, path, query, ...names.map(name => `${name}:${fields[name]}`), '', signed, payload].join('\n');
  const scope = `${date.slice(0, 8)}/${region}/s3/aws4_request`;
  const key = scope.split('/').reduce(mac, `AWS4${secret}`);
  const signature = mac(key, `AWS4-HMAC-SHA256\n${date}\n${scope}\n${sha256(canonical)}`).toString('hex');
  return `AWS4-HMAC-SHA256 Credential=${access}/${scope}, SignedHeaders=${signed}, Signature=${signature}`;
}

module.exports = {
  authorization, bucket, checksum, completionMarker, completionReceipt, initiatedId, initiatedQuery,
  initiationHeaders, initiationReceipt, mapId, mediaKey, sha256, sidecarKey, uploadQuery,
};
