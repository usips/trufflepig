# Token refill investigation

The refill process changes available capacity inside TokenBucket.
An agent must read the complete body, including the middle_capacity_marker
below, rather than inferring behavior from the declaration alone.

The configured queue alias is documented in routing_config.toml.
