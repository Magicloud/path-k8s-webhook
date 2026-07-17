# Path K8S webhook

This is a tool (pkw) to validate / mutate incoming objects, via Json pointers.

## Validation webhook

Pkw can check if the pointer points to a valid node, or the node equals to a value, or equals to a value fetched via another Json pointer from this object or another object.

If one of the two values to compare is an array, the checking can be set as if one contains another.

If both are arrays, the possible checking could be contains, or intersects, or equals.

## Mutation webhook

Another value, or Json pointer (maybe with another object) can be specified to be used in mutation, as set the pointer target to this value.

If validation value is set, a test would be done first, meaning if the pointer target is that value, then set to above value.

## Multiple actions

Multiple validations / mutations can be set to do at once.

In validation, the checks can be set to any passed, all passed, or combined via a boolean expression.
