default_registry("registry.magicloud.lan")
allow_k8s_contexts("default")

jj_revision = str(local('git rev-parse HEAD 2>/dev/null || echo "initial"')).strip()

# nerdctl_build(
#     ref="pkw",
#     context=".",
#     dockerfile="Containerfile",
#     build_args={"BUILD_REVISION": jj_revision}
# )
custom_build(
    ref="pkw",
    command="sudo nerdctl build . -t $EXPECTED_REF --build-arg BUILD_REVISION=" + jj_revision + " && sudo nerdctl push $EXPECTED_REF",
    deps=["."],
    skips_local_docker=True
)

k8s_yaml("webhook.yaml")
k8s_resource(
    workload="path-k8s-webhook",
    trigger_mode=TRIGGER_MODE_MANUAL
)

local_resource(
    name="test",
    cmd="kubectl apply -f ../server/k3s/test.yaml && exit 1 || exit 0",
    trigger_mode=TRIGGER_MODE_MANUAL,
    resource_deps=["path-k8s-webhook"]
)