use std::{
    collections::{BTreeMap, HashSet},
    fmt::{Debug, Display},
    future::ready,
    io::Read,
    pin::Pin,
    sync::Arc,
    time::Duration,
};

use async_drop::AsyncDrop;
use clap::Parser;
use eyre::{OptionExt, Report, Result, eyre};
use futures::{AsyncBufRead, AsyncBufReadExt, Stream, StreamExt, TryStreamExt, stream};
use k8s_openapi::{
    api::{
        apps::v1::{Deployment, ReplicaSet},
        core::v1::{ContainerState, Namespace, Pod, Probe},
        discovery::v1::EndpointSlice,
    },
    apimachinery::pkg::apis::meta::v1::{LabelSelector, LabelSelectorRequirement},
    serde::de::DeserializeOwned,
};
use kube::{
    Api, Client, Resource, ResourceExt,
    api::{
        ApiResource, DeleteParams, DynamicObject, GroupVersionKind, ListParams, LogParams,
        ObjectMeta, Patch, PatchParams, PostParams, WatchEvent, WatchParams,
    },
    core::response::reason,
    discovery::Scope,
    runtime::watcher::watch_object,
};
use libtest_mimic::{Arguments, Failed};
use tokio::{runtime::LocalRuntime, time::timeout};
use yaml_split::DocumentIterator;

use crate::{DISCOVERY, PERSIST_TEST_ARGS};

#[derive(Clone)]
pub struct SingleThreadMutPtr<T>(pub *mut T);
unsafe impl<T> Send for SingleThreadMutPtr<T> {}
unsafe impl<T> Sync for SingleThreadMutPtr<T> {}

#[derive(Parser, Debug)]
pub struct ITArgs {
    #[command(flatten)]
    pub mimic_args: Arguments,
    #[arg(short, long)]
    pub image_sha256: String,
    #[arg(short, long)]
    pub namespace: String,
}

pub async fn create_many_in_namespace<'a>(
    dos: &'a [DynamicObject],
    namespace: &str,
    client: Client,
) -> Result<(), (Report, Vec<&'a DynamicObject>)> {
    let iter = get_api(dos, namespace, client);
    let mut done = Vec::new();
    for (d_o, api) in iter {
        match api {
            Ok(api) => match api.create(&PostParams::default(), d_o).await {
                Ok(_) => done.push(d_o),
                Err(e) => return Err((e.into(), done)),
            },
            Err(e) => return Err((e, done)),
        }
    }
    Ok(())
}

pub fn read_to_dynamic_objects(manifests: impl Read) -> Result<Vec<DynamicObject>> {
    DocumentIterator::new(manifests)
        .map(|yaml_doc| {
            let yaml_doc = yaml_doc?;
            let d_o: DynamicObject = serde_yaml_ng::from_str(&yaml_doc)?;
            Ok(d_o)
        })
        .collect()
}

pub async fn delete_many_in_namespace_force<'a>(
    dos: &'a [DynamicObject],
    namespace: &str,
    client: Client,
) -> Vec<(&'a DynamicObject, Report)> {
    let mut iter = get_api(dos, namespace, client).collect::<Vec<_>>();
    iter.reverse();
    let mut results = Vec::new();
    for (d_o, api) in iter {
        match api {
            Ok(api) => {
                dbg!(d_o.name_any());
                let x = api
                    .delete(&d_o.name_any(), &DeleteParams::background())
                    .await;
                // .map(|x| {
                //     x.map_left(|o| eprintln!("Deleting CRD: {:?}", o.data.get("status")))
                //         .map_right(|s| eprintln!("Deleted CRD: {s:?}"))
                // });
                if let Err(e) = x {
                    results.push((d_o, e.into()));
                }
            }
            Err(e) => results.push((d_o, e)),
        }
    }
    results
}

pub fn get_api<'a>(
    dos: &'a [DynamicObject],
    namespace: &str,
    client: Client,
) -> impl Iterator<Item = (&'a DynamicObject, Result<Api<DynamicObject>>)> {
    let dis = DISCOVERY.get().unwrap();
    dos.iter().filter_map(move |d_o| {
        d_o.types
            .as_ref()
            .map(|types| {
                let (gvk, scope) = dis
                    .groups()
                    .find_map(|g| {
                        g.resources_by_stability()
                            .into_iter()
                            .find(|(ar, _)| ar.kind.eq_ignore_ascii_case(&types.kind))
                            .map(|(ar, ac)| {
                                (
                                    GroupVersionKind::gvk(g.name(), &ar.version, &ar.kind),
                                    ac.scope,
                                )
                            })
                    })
                    .ok_or_eyre(format!("{} does not exist", types.kind))?;
                let ar = ApiResource::from_gvk(&gvk);
                let api: Api<DynamicObject> = match scope {
                    Scope::Cluster => Api::all_with(client.clone(), &ar),
                    Scope::Namespaced => Api::namespaced_with(
                        client.clone(),
                        d_o.metadata.namespace.as_ref().map_or(namespace, |v| v),
                        &ar,
                    ),
                };
                Ok(api) as Result<_>
            })
            .map(|r| (d_o, r))
    })
}

pub struct HumanReadableId<'a> {
    pub kind: &'a str,
    pub namespace: &'a Option<String>,
    pub name: &'a str,
}
impl Display for HumanReadableId<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let empty_str = String::new();
        f.write_str(&format!(
            "{} {}/{}",
            self.kind,
            self.namespace.as_ref().unwrap_or(&empty_str),
            self.name
        ))
    }
}
impl<'a> TryFrom<&'a DynamicObject> for HumanReadableId<'a> {
    type Error = eyre::Error;

    fn try_from(value: &'a DynamicObject) -> Result<Self, Self::Error> {
        Ok(Self {
            kind: &value.types.as_ref().ok_or_else(|| eyre!(""))?.kind,
            namespace: &value.metadata.namespace,
            name: value.metadata.name.as_ref().ok_or_else(|| eyre!(""))?,
        })
    }
}

pub struct Test<'a> {
    pub name: &'a str,
    pub argument: Vec<&'a str>,
    pub pass: Vec<DynamicObject>,
    pub fail: Vec<DynamicObject>,
}

pub struct Guard<'a> {
    pub created_namespace: &'a bool,
    pub namespace: &'a str,
    pub everything: &'a [DynamicObject],
}
impl AsyncDrop for Guard<'_> {
    fn async_drop(&mut self) -> async_drop::AsyncDropFuture<'_> {
        Box::pin(async {
            eprintln!("Async Drop");
            // Due to AsyncDrop mem::swap Dummy with Guard,
            // if used a cloned Client, its Future will not be Waker-ed.
            // This would result in hang.
            let client = Client::try_default().await.map_err(|e| format!("{e:?}"))?;
            for (d_o, e) in
                delete_many_in_namespace_force(self.everything, self.namespace, client.clone())
                    .await
            {
                eprintln!("{}: {e:?}", HumanReadableId::try_from(d_o).unwrap());
            }
            if *self.created_namespace {
                let namespaces: Api<Namespace> = Api::all(client.clone());
                namespaces
                    .delete(self.namespace, &DeleteParams::default())
                    .await
                    .map_err(|e| format!("{e:?}"))?;
            }
            Ok(())
        })
    }
}

pub async fn is_service_ready(ns: &str, client: Client) -> Result<bool> {
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), ns);
    let pods: Api<Pod> = Api::namespaced(client.clone(), ns);
    let endpointslices: Api<EndpointSlice> = Api::namespaced(client.clone(), ns);

    let pod_names = deployments.get_current_pods("path-k8s-webhook").await?;
    let pod_ips = stream::iter(pod_names)
        .then(async |pn| pods.get_status(&pn).await)
        .try_filter_map(|p| ready(Ok(p.status.and_then(|s| s.pod_ip))))
        .try_collect::<Vec<_>>()
        .await?;
    let pod_ips = pod_ips.iter().collect::<HashSet<_>>();
    let svc_ips = endpointslices
        .list(&ListParams {
            label_selector: Some("kubernetes.io/service-name=path-k8s-webhook".to_string()),
            ..Default::default()
        })
        .await?;
    let svc_ips = svc_ips
        .iter()
        .flat_map(|el| el.endpoints.iter().flat_map(|e| e.addresses.iter()))
        .collect::<HashSet<_>>();

    Ok(pod_ips.is_superset(&svc_ips))
}

pub async fn patch_confirm_pod(x: &Deployment, ns: &str, client: Client) -> Result<()> {
    // Not all containers have to have readiness
    // Once those are ready and others are running
    // The pod is ready
    let has_readiness = get_readiness_probes(x).is_some_and(|mut iter| iter.next().is_some());

    let deployments: Api<Deployment> = Api::namespaced(client.clone(), ns);
    let result_deployment = deployments
        .patch(
            "path-k8s-webhook",
            &PatchParams::apply("pkw-it").force(),
            &Patch::Apply(x),
        )
        .await?;

    wait_until_first_pod_change(&result_deployment, &deployments).await?;
    let pod_name = deployments
        .get_current_pods("path-k8s-webhook")
        .await?
        .next()
        .expect("Cannot get pod matching the template");
    let pods: Api<Pod> = Api::namespaced(client.clone(), ns);

    // dbg!(
    //     pods.get(&pod_name).await?.spec.unwrap().containers[0]
    //         .args
    //         .as_ref()
    //         .unwrap()
    // );
    // tokio::time::sleep(Duration::from_secs(5)).await;
    // dbg!(
    //     pods.get(&pod_name).await?.spec.unwrap().containers[0]
    //         .args
    //         .as_ref()
    //         .unwrap()
    // );

    if has_readiness {
        eprintln!("Not yet");
    } else {
        // streaming container names for log
        let events = watch_object(pods.clone(), &pod_name)
            .map_err(eyre::Error::from)
            .try_filter_map(|x| {
                let try_block = || {
                    Some(
                        x?.status?
                            .container_statuses?
                            .into_iter()
                            .filter_map(|x| x.state),
                    )
                };
                ready(Ok(try_block().map(|x| {
                    stream::iter(x.into_iter().map(|y| Ok(y) as Result<ContainerState>))
                })))
            })
            .try_flatten()
            .and_then(async |cs| {
                let ret = ContainerStatus::from(&cs);
                if ret == ContainerStatus::Crashing {
                    eprintln!(">>> Pod log ===");
                    let mut logs = pods
                        .log_stream(
                            &pod_name,
                            &LogParams {
                                previous: true,
                                ..Default::default()
                            },
                        )
                        .await?
                        .lines();
                    while let Some(line) = logs.try_next().await? {
                        eprintln!("{line}");
                    }
                    eprintln!("<<< Pod log ===");
                } else if ret == ContainerStatus::Terminated {
                    eprintln!(">>> Pod log ===");
                    let mut logs = pods
                        .log_stream(&pod_name, &LogParams::default())
                        .await?
                        .lines();
                    while let Some(line) = logs.try_next().await? {
                        eprintln!("{line}");
                    }
                    eprintln!("<<< Pod log ===");
                }
                Ok(ret)
            })
            .try_skip_while(|x| ready(Ok(*x == ContainerStatus::Creating)));
        let mut events = Box::pin(events);
        match events.try_next().await? {
            Some(ContainerStatus::Running) => {
                // see if we got crashing in 5 secs.
                match timeout(
                    Duration::from_secs(5),
                    events.try_any(|x| ready(x != ContainerStatus::Running)),
                )
                .await
                {
                    Ok(Ok(true)) => {
                        // Crashing
                        Err(eyre! {"Pod is crashing"})?;
                    }
                    Ok(Ok(false)) => {
                        // Not crashing, but stream closed.
                        Err(eyre!("Pod event stream closed"))?;
                    }
                    Ok(Err(e)) => Err(e)?,
                    Err(_) => {
                        // No crashing in 5 secs, all good.
                    }
                }
            }
            Some(_) => Err(eyre!("Pod is not ready"))?,
            None => Err(eyre!("Pod event stream closed"))?,
        }
    }

    Ok(())
}

pub async fn get_pod_name_from_template_hash(
    template_hash: String,
    pod: Api<Pod>,
) -> Result<impl Iterator<Item = String>, kube::Error> {
    let p = pod
        .list(&ListParams {
            label_selector: Some(format!(
                "app=path-k8s-webhook,pod-template-hash={template_hash}"
            )),
            ..Default::default()
        })
        .await?;
    let pod_names = p.into_iter().map(|x| x.name_any());
    Ok(pod_names)
}

pub fn get_readiness_probes(deployment: &Deployment) -> Option<impl Iterator<Item = &Probe>> {
    Some(
        deployment
            .spec
            .as_ref()?
            .template
            .spec
            .as_ref()?
            .containers
            .iter()
            .filter_map(|c| c.readiness_probe.as_ref()),
    )
}

pub async fn get_current_pod_template_hash(
    deployment_name: &str,
    namespace: &str,
    client: Client,
) -> Result<String, kube::Error> {
    let deployment: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let md = deployment.get_metadata(deployment_name).await?;
    let rev = md
        .annotations()
        .get("deployment.kubernetes.io/revision")
        .expect("Cannot get deployment revision");

    let replicaset: Api<ReplicaSet> = Api::namespaced(client.clone(), namespace);
    let rss = replicaset
        .list_metadata(&ListParams {
            label_selector: Some("app=path-k8s-webhook".to_string()),
            ..Default::default()
        })
        .await?;
    let rs_md = rss
        .iter()
        .find(|rs| {
            rs.annotations()
                .get("deployment.kubernetes.io/revision")
                .expect("Cannot get replicaset revision")
                == rev
        })
        .expect("Cannot find matching replicaset");
    let pod_template_hash = rs_md
        .labels()
        .get("pod-template-hash")
        .expect("Cannot get replicaset pod-template-hash");
    Ok(pod_template_hash.clone())
}

pub async fn wait_until_first_pod_change(
    deployment: &Deployment,
    api: &Api<Deployment>,
) -> Result<(), Report> {
    let rv = deployment
        .meta()
        .resource_version
        .as_ref()
        .expect("The deployment does not contain a resource version");
    let get_conditions = |event: Option<Option<Deployment>>| event??.status?.conditions;
    let mut watcher = Box::pin(watch_object_by_version(api, &deployment.name_any(), rv).await?);
    loop {
        if let Some(conditions) = get_conditions(watcher.try_next().await?)
            && conditions.into_iter().any(|c| {
                c.type_.eq_ignore_ascii_case("Progressing")
                    && c.reason
                        .is_some_and(|r| r.eq_ignore_ascii_case("ReplicaSetUpdated"))
            })
        {
            break;
        }
    }
    Ok(())
}

#[derive(PartialEq, Eq)]
pub enum ContainerStatus {
    Creating,
    Running,
    Crashing,
    Terminated,
    Others,
}
impl From<&ContainerState> for ContainerStatus {
    fn from(cs: &ContainerState) -> Self {
        if let Some(w) = cs.waiting.as_ref()
            && let Some(r) = w.reason.as_ref()
        {
            if r.eq_ignore_ascii_case("containercreating") {
                Self::Creating
            } else if r.eq_ignore_ascii_case("crashloopbackoff") {
                Self::Crashing
            } else {
                eprintln!("What else: {r}"); // Error?
                Self::Others
            }
        } else if cs.running.is_some() {
            Self::Running
        } else if cs.terminated.is_some() {
            Self::Terminated
        } else {
            unimplemented!()
        }
    }
}

pub async fn watch_object_by_version<K>(
    api: &Api<K>,
    name: &str,
    resource_version: &str,
) -> Result<impl Stream<Item = Result<Option<K>>> + Send + use<K>>
where
    K: Resource + Clone + DeserializeOwned + Debug + Send + 'static,
{
    let x = api
        .watch(
            &WatchParams {
                field_selector: Some(format!("metadata.name={name}")),
                ..Default::default()
            },
            resource_version,
        )
        .await?
        .map_err(eyre::Error::from)
        .and_then(|we| {
            ready(match we {
                WatchEvent::Added(k) | WatchEvent::Modified(k) => Ok(Some(k)),
                WatchEvent::Deleted(_) => Ok(None),
                WatchEvent::Bookmark(_bookmark) => {
                    eprintln!("bookmark");
                    Ok(None)
                }
                WatchEvent::Error(status) => Err(eyre!("{status:?}")),
            })
        });
    Ok(x)
}

pub fn do_the_work(
    webhook: &Deployment,
    args: &SingleThreadMutPtr<Vec<String>>,
    test: &Test<'_>,
    namespace: &str,
    client: &Client,
    show_log: bool,
) -> Result<(), Failed> {
    LocalRuntime::new()?.block_on(async {
        let args = args.0;
        unsafe {
            // borrow checker sucks
            *args = PERSIST_TEST_ARGS
                .iter()
                .chain(test.argument.iter())
                .map(std::string::ToString::to_string)
                .collect();
        }

        let deployment_name = webhook.name_any();
        patch_confirm_pod(webhook, namespace, client.clone()).await?;
        while !is_service_ready(namespace, client.clone()).await? {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }

        let deployments: Api<Deployment> = Api::namespaced(client.clone(), namespace);
        let run_test = async |test| {
            let ret = create_many_in_namespace(test, namespace, client.clone()).await;
            delete_many_in_namespace_force(test, namespace, client.clone()).await;
            ret
        };

        if !test.pass.is_empty() {
            eprintln!("should pass");
            let r = run_test(&test.pass).await;
            if show_log {
                eprintln!(">>> Pod log ===");
                let lp = LogParams::default();
                let mut logs = deployments
                    .log_stream_ext(&deployment_name, &lp)
                    .await?
                    .lines();
                while let Some(line) = logs.try_next().await? {
                    eprintln!("{line}");
                }
                eprintln!("<<< Pod log ===");
            }
            assert!(
                r.is_ok(),
                "Tests should pass failed for \n{}",
                r.unwrap_err().0
            );
        }

        if !test.fail.is_empty() {
            eprintln!("should fail");
            let r = run_test(&test.fail).await;
            if show_log {
                eprintln!(">>> Pod log ===");
                let lp = LogParams::default();
                let mut logs = deployments
                    .log_stream_ext(&deployment_name, &lp)
                    .await?
                    .lines();
                while let Some(line) = logs.try_next().await? {
                    eprintln!("{line}");
                }
                eprintln!("<<< Pod log ===");
            }
            assert!(r.is_err(), "Tests should fail passed");
        }

        Ok(())
    })
}

pub type Infra = Vec<DynamicObject>;
pub type WrappedDeployment = Arc<Pin<Box<Deployment>>>;
pub type ArgsPtr = SingleThreadMutPtr<Vec<String>>;
/// .
///
/// # Panics
///
/// Panics if .
///
/// # Errors
///
/// This function will return an error if .
pub fn prepare_infra(image_sha256: &str) -> Result<(Infra, WrappedDeployment, ArgsPtr)> {
    let infra_file = include_bytes!("../infra.yaml");
    let infra = read_to_dynamic_objects(infra_file.as_slice())?;
    let webhook: Deployment = infra
        .iter()
        .find_map(|d_o| {
            if HumanReadableId::try_from(d_o).unwrap().to_string() == "Deployment /path-k8s-webhook"
            {
                Some(serde_yaml_ng::to_value(d_o).and_then(serde_yaml_ng::from_value))
            } else {
                None
            }
        })
        .unwrap()?;
    let mut webhook = Box::pin(webhook);
    if let Some(image) = webhook
        .spec
        .as_mut()
        .and_then(|spec| spec.template.spec.as_mut())
        .and_then(|spec| spec.containers.get_mut(0))
        .and_then(|container| container.image.as_mut())
    {
        *image = format!("{image}@{image_sha256}");
    }
    let args: *mut Vec<String> = webhook
        .spec
        .as_mut()
        .and_then(|spec| spec.template.spec.as_mut())
        .and_then(|spec| spec.containers.get_mut(0))
        .and_then(|container| container.args.as_mut())
        .map(std::ptr::from_mut::<Vec<String>>)
        .expect("Cannot point to the args node");
    let args = SingleThreadMutPtr(args);
    let webhook = Arc::new(webhook);

    Ok((infra, webhook, args))
}

pub async fn confirm_namespace(namespace: &str, client: Client) -> Result<bool> {
    let namespaces: Api<Namespace> = Api::all(client);
    if namespaces.get_metadata_opt(namespace).await?.is_none() {
        namespaces
            .create(
                &PostParams::default(),
                &Namespace {
                    metadata: ObjectMeta {
                        name: Some(namespace.to_owned()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            )
            .await?;
        Ok(true)
    } else {
        Ok(false)
    }
}

pub trait ApiExt {
    async fn log_stream_ext(
        &self,
        name: &str,
        lp: &LogParams,
    ) -> Result<impl AsyncBufRead, kube::Error>;
    // async fn exec_ext<I, T>(
    //     &self,
    //     name: &str,
    //     command: I,
    //     ap: &AttachParams,
    // ) -> Result<AttachedProcess, kube::Error>
    // where
    //     I: IntoIterator<Item = T> + Debug,
    //     T: Into<String>;
    async fn get_current_pods(
        &self,
        name: &str,
    ) -> Result<impl Iterator<Item = String>, kube::Error>;
}
impl ApiExt for Api<Deployment> {
    async fn log_stream_ext(
        &self,
        name: &str,
        lp: &LogParams,
    ) -> Result<impl AsyncBufRead, kube::Error> {
        let client = self.clone().into_client();
        let ns = self.namespace().expect("");
        let pods: Api<Pod> = Api::namespaced(client, ns);

        let deployment = self.get(name).await?;
        let label_selector = deployment.spec.map(|s| s.selector.to_k8s_label_selector());
        dbg!(&label_selector);

        let first_pod = pods
            .list_metadata(&ListParams {
                label_selector,
                ..Default::default()
            })
            .await?;
        let first_pod = first_pod.iter().next().ok_or(kube::Error::Api(
            kube::error::Status {
                code: 404,
                reason: reason::NOT_FOUND.to_string(),
                ..Default::default()
            }
            .into(),
        ))?;

        pods.log_stream(&first_pod.name_any(), lp).await
    }

    // async fn exec_ext<I, T>(
    //     &self,
    //     name: &str,
    //     command: I,
    //     ap: &AttachParams,
    // ) -> Result<AttachedProcess, kube::Error>
    // where
    //     I: IntoIterator<Item = T> + Debug,
    //     T: Into<String>,
    // {
    //     let client = self.clone().into_client();
    //     let ns = self.namespace().expect("");
    //     let pods: Api<Pod> = Api::namespaced(client, ns);

    //     let deployment = self.get(name).await?;
    //     let label_selector = deployment.spec.map(|s| s.selector.to_k8s_label_selector());
    //     dbg!(&label_selector);

    //     let first_pod = pods
    //         .list_metadata(&ListParams {
    //             label_selector,
    //             ..Default::default()
    //         })
    //         .await?;
    //     let first_pod = first_pod.iter().next().ok_or(kube::Error::Api(
    //         kube::error::Status {
    //             code: 404,
    //             reason: reason::NOT_FOUND.to_string(),
    //             ..Default::default()
    //         }
    //         .into(),
    //     ))?;

    //     pods.exec(&first_pod.name_any(), command, ap).await
    // }

    async fn get_current_pods(
        &self,
        name: &str,
    ) -> Result<impl Iterator<Item = String>, kube::Error> {
        let client = self.clone().into_client();
        let ns = self.namespace().expect("msg");
        let pods: Api<Pod> = Api::namespaced(client.clone(), ns);

        let pod_template_hash = get_current_pod_template_hash(name, ns, client.clone()).await?;

        get_pod_name_from_template_hash(pod_template_hash, pods).await
    }
}

pub trait ToK8SLabelSelector {
    fn to_k8s_label_selector(&self) -> String;
}
impl ToK8SLabelSelector for LabelSelectorRequirement {
    fn to_k8s_label_selector(&self) -> String {
        let empty_vec = vec![];
        if self.operator.eq_ignore_ascii_case("in") || self.operator.eq_ignore_ascii_case("notin") {
            format!(
                "{} {} ({})",
                self.key,
                self.operator,
                self.values.as_ref().unwrap_or(&empty_vec).join(", ")
            )
        } else if self.operator.eq_ignore_ascii_case("exists") {
            self.key.clone()
        } else if self.operator.eq_ignore_ascii_case("doesnotexist") {
            format!("!{}", self.key)
        } else {
            unimplemented!()
        }
    }
}
impl ToK8SLabelSelector for Vec<LabelSelectorRequirement> {
    fn to_k8s_label_selector(&self) -> String {
        self.iter()
            .map(ToK8SLabelSelector::to_k8s_label_selector)
            .collect::<Vec<_>>()
            .join(",")
    }
}
impl ToK8SLabelSelector for BTreeMap<String, String> {
    fn to_k8s_label_selector(&self) -> String {
        self.iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(",")
    }
}
impl ToK8SLabelSelector for LabelSelector {
    fn to_k8s_label_selector(&self) -> String {
        [
            self.match_expressions
                .as_ref()
                .map(ToK8SLabelSelector::to_k8s_label_selector),
            self.match_labels
                .as_ref()
                .map(ToK8SLabelSelector::to_k8s_label_selector),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(",")
    }
}

pub fn regulate_infra<'a>(
    infra: impl IntoIterator<Item = &'a mut DynamicObject>,
    namespace: &str,
) -> Option<()> {
    for d_o in infra {
        match d_o.types.as_ref()?.kind.to_ascii_uppercase().as_str() {
            "CLUSTERROLEBINDING" => {
                d_o.data["subjects"]
                    .as_array_mut()?
                    .iter_mut()
                    .for_each(|s| s["namespace"] = namespace.into());
            }
            "DEPLOYMENT" => {
                d_o.data["spec"]["template"]["spec"]["volumes"].as_array_mut()?.iter_mut().for_each(|v| {
                    if let Some(csi) = v.get_mut("csi")
                    && let Some(vas) = csi.get_mut("volumeAttributes")
                    && let Some(v) = vas.get_mut("csi.cert-manager.io/dns-names") {
                        *v = format!("path-k8s-webhook.{namespace}.svc,path-k8s-webhook.{namespace}.svc.cluster.local").into();
                    }
                });
            }
            "VALIDATINGWEBHOOKCONFIGURATION" => {
                d_o.data["webhooks"]
                    .as_array_mut()?
                    .iter_mut()
                    .for_each(|w| {
                        w["namespaceSelector"]["matchLabels"]["kubernetes.io/metadata.name"] =
                            namespace.into();
                        w["clientConfig"]["service"]["namespace"] = namespace.into();
                    });
            }
            _ => (),
        }
    }
    Some(())
}
