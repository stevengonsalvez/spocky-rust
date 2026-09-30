//! Offline Hub billing catalog and provisioning boundary pilot.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PriceInterval {
    Monthly,
    Annual,
}

impl PriceInterval {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Monthly => "monthly",
            Self::Annual => "annual",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BillingPrice {
    pub id: String,
    pub lookup_key: String,
    pub interval: PriceInterval,
    pub unit_amount: u64,
    pub currency: String,
    pub active: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BillingTemplate {
    pub seat_max: Option<u64>,
    pub can_invite_members: bool,
    pub executions_per_month: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BillingPlan {
    pub id: String,
    pub slug: String,
    pub name: String,
    pub template: BillingTemplate,
    pub features: Vec<(String, String, Option<String>)>,
    pub monthly_tooltip: Option<String>,
    pub annual_tooltip: Option<String>,
    pub active: bool,
    pub prices: Vec<BillingPrice>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicBillingPrice {
    pub interval: PriceInterval,
    pub interval_count: u8,
    pub unit_amount: u64,
    pub currency: String,
    pub tooltip: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicBillingPlan {
    pub slug: String,
    pub name: String,
    pub included_seats: Option<u64>,
    pub included_executions_per_month: Option<u64>,
    pub features: Vec<(String, String, Option<String>)>,
    pub prices: Vec<PublicBillingPrice>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProvisioningEntitlement {
    pub plan_id: Option<String>,
    pub granted: BillingTemplate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PriceSelectionError {
    Ambiguous {
        slug: String,
        interval: PriceInterval,
    },
}

#[must_use]
pub fn expected_lookup_key(slug: &str, interval: PriceInterval) -> String {
    format!("{slug}_{}", interval.as_str())
}

pub fn select_active_price(
    prices: &[BillingPrice],
    slug: &str,
    interval: PriceInterval,
) -> Result<Option<BillingPrice>, PriceSelectionError> {
    let lookup_key = expected_lookup_key(slug, interval);
    let mut matching = prices
        .iter()
        .filter(|price| price.active && price.lookup_key == lookup_key);
    let selected = matching.next().cloned();
    if matching.next().is_some() {
        return Err(PriceSelectionError::Ambiguous {
            slug: slug.to_owned(),
            interval,
        });
    }
    Ok(selected)
}

#[derive(Clone, Debug)]
pub struct BillingCatalog {
    plans: Vec<BillingPlan>,
}

impl BillingCatalog {
    pub fn new(plans: impl IntoIterator<Item = BillingPlan>) -> Self {
        Self {
            plans: plans.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn public_plans(&self) -> Vec<PublicBillingPlan> {
        self.plans
            .iter()
            .filter(|plan| plan.active)
            .map(|plan| PublicBillingPlan {
                slug: plan.slug.clone(),
                name: plan.name.clone(),
                included_seats: plan.template.seat_max,
                included_executions_per_month: plan.template.executions_per_month,
                features: plan.features.clone(),
                prices: [PriceInterval::Monthly, PriceInterval::Annual]
                    .into_iter()
                    .filter_map(|interval| {
                        select_active_price(&plan.prices, &plan.slug, interval)
                            .ok()
                            .flatten()
                            .map(|price| PublicBillingPrice {
                                interval,
                                interval_count: 1,
                                unit_amount: price.unit_amount,
                                currency: price.currency,
                                tooltip: match interval {
                                    PriceInterval::Monthly => plan.monthly_tooltip.clone(),
                                    PriceInterval::Annual => plan.annual_tooltip.clone(),
                                },
                            })
                    })
                    .collect(),
            })
            .collect()
    }

    #[must_use]
    pub fn provisioning_entitlement(&self) -> ProvisioningEntitlement {
        if let Some(plan) = self
            .plans
            .iter()
            .find(|plan| plan.active && plan.slug == "free")
        {
            return ProvisioningEntitlement {
                plan_id: Some(plan.id.clone()),
                granted: plan.template.clone(),
            };
        }
        ProvisioningEntitlement {
            plan_id: None,
            granted: BillingTemplate {
                seat_max: Some(1),
                can_invite_members: false,
                executions_per_month: Some(50),
            },
        }
    }
}
