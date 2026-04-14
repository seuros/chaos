# chaos-abi

The canonical internal error and message types. Every provider translates into
this format — never from one provider wire format to another. When adding a new
error condition, define it here first, then wire it outward to the provider layer.
