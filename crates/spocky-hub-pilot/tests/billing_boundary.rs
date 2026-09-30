use spocky_hub_pilot::billing::{
    BillingCatalog, BillingPlan, BillingPrice, BillingTemplate, PriceInterval, PriceSelectionError,
    PublicBillingPlan, select_active_price,
};

fn free_plan() -> BillingPlan {
    BillingPlan {
        id: "prod_free".into(),
        slug: "free".into(),
        name: "Free".into(),
        template: BillingTemplate {
            seat_max: Some(1),
            can_invite_members: false,
            executions_per_month: Some(50),
        },
        features: vec![(
            "feature-1".into(),
            "Daemons run on your machines".into(),
            None,
        )],
        monthly_tooltip: None,
        annual_tooltip: None,
        active: true,
        prices: vec![BillingPrice {
            id: "price_free_monthly".into(),
            lookup_key: "free_monthly".into(),
            interval: PriceInterval::Monthly,
            unit_amount: 0,
            currency: "usd".into(),
            active: true,
        }],
    }
}

fn hosted_plan() -> BillingPlan {
    BillingPlan {
        id: "prod_hosted".into(),
        slug: "hosted".into(),
        name: "Spocky Hub".into(),
        template: BillingTemplate {
            seat_max: None,
            can_invite_members: true,
            executions_per_month: None,
        },
        features: vec![(
            "feature-1".into(),
            "Unlimited daemons".into(),
            Some("Connect any number of development machines.".into()),
        )],
        monthly_tooltip: Some("$15 per seat, billed monthly.".into()),
        annual_tooltip: None,
        active: true,
        prices: vec![BillingPrice {
            id: "price_hosted_monthly".into(),
            lookup_key: "hosted_monthly".into(),
            interval: PriceInterval::Monthly,
            unit_amount: 1500,
            currency: "usd".into(),
            active: true,
        }],
    }
}

#[test]
fn public_catalog_matches_frozen_plan_boundary() {
    let catalog = BillingCatalog::new([free_plan(), hosted_plan()]);

    assert_eq!(
        catalog.public_plans(),
        vec![
            PublicBillingPlan {
                slug: "free".into(),
                name: "Free".into(),
                included_seats: Some(1),
                included_executions_per_month: Some(50),
                features: vec![(
                    "feature-1".into(),
                    "Daemons run on your machines".into(),
                    None,
                )],
                prices: vec![spocky_hub_pilot::billing::PublicBillingPrice {
                    interval: PriceInterval::Monthly,
                    interval_count: 1,
                    unit_amount: 0,
                    currency: "usd".into(),
                    tooltip: None,
                }],
            },
            PublicBillingPlan {
                slug: "hosted".into(),
                name: "Spocky Hub".into(),
                included_seats: None,
                included_executions_per_month: None,
                features: vec![(
                    "feature-1".into(),
                    "Unlimited daemons".into(),
                    Some("Connect any number of development machines.".into()),
                )],
                prices: vec![spocky_hub_pilot::billing::PublicBillingPrice {
                    interval: PriceInterval::Monthly,
                    interval_count: 1,
                    unit_amount: 1500,
                    currency: "usd".into(),
                    tooltip: Some("$15 per seat, billed monthly.".into()),
                }],
            },
        ]
    );
}

#[test]
fn catalog_withholds_inactive_plans_and_nonmatching_prices() {
    let mut inactive = hosted_plan();
    inactive.active = false;
    let mut wrong_key = free_plan();
    wrong_key.prices[0].lookup_key = "legacy_monthly".into();

    let plans = BillingCatalog::new([inactive, wrong_key]).public_plans();
    assert_eq!(plans.len(), 1);
    assert_eq!(plans[0].slug, "free");
    assert!(plans[0].prices.is_empty());
}

#[test]
fn catalog_rejects_ambiguous_exact_price_identity() {
    let mut plan = hosted_plan();
    plan.prices.push(BillingPrice {
        id: "price_duplicate".into(),
        ..plan.prices[0].clone()
    });

    assert_eq!(
        select_active_price(&plan.prices, &plan.slug, PriceInterval::Monthly),
        Err(PriceSelectionError::Ambiguous {
            slug: "hosted".into(),
            interval: PriceInterval::Monthly,
        })
    );
    assert!(
        BillingCatalog::new([plan]).public_plans()[0]
            .prices
            .is_empty()
    );
}

#[test]
fn provisioning_uses_free_plan_or_conservative_floor() {
    let free = BillingCatalog::new([free_plan()]).provisioning_entitlement();
    assert_eq!(free.plan_id.as_deref(), Some("prod_free"));
    assert_eq!(free.granted.seat_max, Some(1));
    assert!(!free.granted.can_invite_members);
    assert_eq!(free.granted.executions_per_month, Some(50));

    let missing = BillingCatalog::new([hosted_plan()]).provisioning_entitlement();
    assert_eq!(missing.plan_id, None);
    assert_eq!(missing.granted.seat_max, Some(1));
    assert!(!missing.granted.can_invite_members);
    assert_eq!(missing.granted.executions_per_month, Some(50));

    let mut inactive = free_plan();
    inactive.active = false;
    assert_eq!(
        BillingCatalog::new([inactive])
            .provisioning_entitlement()
            .plan_id,
        None
    );
}
