-- Task 8: `GET /api/me` and `GET /api/tenants` look memberships up by subject.
CREATE INDEX member_sub_idx ON member (sub);
