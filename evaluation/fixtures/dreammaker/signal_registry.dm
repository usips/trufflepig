#define COMSIG_BUCKET_REFILL "bucket_refill"
/datum/token_bucket
    var/available = 0
    proc/refill_tokens(amount)
        available += amount
        SEND_SIGNAL(src, COMSIG_BUCKET_REFILL, amount)
    verb/inspect_bucket()
        return available
/datum/token_bucket/refill_tokens(amount)
    return ..(amount)
/datum/special_bucket
    parent_type = /datum/token_bucket
    proc/register_refill()
        RegisterSignal(src, COMSIG_BUCKET_REFILL, PROC_REF(on_refill))
    proc/on_refill(datum/source, amount)
        return amount
// /datum/fake/proc/comment_only() is not a declaration.
