-- How independent an agent review was (plan §2.3 "Reviewers", coordinator
-- task 6cf630c0): distinct_launch, distinct_host or distinct_vendor, as the
-- posting supervisor reports it. NULL for reviews recorded without one.
ALTER TABLE review_decisions ADD COLUMN review_independence TEXT;
