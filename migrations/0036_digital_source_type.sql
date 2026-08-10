-- AI-disclosure: IPTC Digital Source Type on a story (EU AI Act Art. 50 machine-
-- readable marking). NULL = unspecified; otherwise an IPTC digitalsourcetype code
-- (e.g. `digitalCapture`, `trainedAlgorithmicMedia`) that travels in the ninjs /
-- NewsML-G2 feeds. Values are validated at the API against contracts::DIGITAL_SOURCE_TYPES.
ALTER TABLE meridian.stories ADD COLUMN digital_source_type text;
