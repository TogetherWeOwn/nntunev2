SELECT r.mbid::text AS mbid, r.title AS label, similarity(r.title, $1) AS score,
       r.artist_mbid::text AS artist_mbid, a.name AS artist_name
FROM mb_release r LEFT JOIN mb_artist a ON a.mbid = r.artist_mbid
WHERE r.title % $1
ORDER BY score DESC, label ASC, r.mbid ASC
