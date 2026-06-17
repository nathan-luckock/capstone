//! Roles, privileges, and ownership.
//!
//! picklejar authorizes statements against a [`SecurityCatalog`]: a set of named
//! roles (users and groups), the table privileges granted to them, role
//! membership, and per-table ownership. A session runs as a *current role*; a
//! statement is allowed when that role is a superuser, owns the object, or holds
//! (directly, through `PUBLIC`, or through a group it belongs to) the privilege
//! the statement needs.
//!
//! # Open by default
//!
//! A fresh database has one bootstrap superuser ([`BOOTSTRAP_SUPERUSER`]) and no
//! other roles. The default session runs as that superuser, which bypasses every
//! check, so an unconfigured database behaves exactly as before roles existed.
//! Enforcement begins only once an administrator creates roles and a session
//! runs as a non-superuser (see [`Database::set_session_user`]).

use std::collections::{HashMap, HashSet};

use picklejar_sql::statement::Privilege;

/// The role every fresh database starts with: a superuser that owns nothing and
/// bypasses all permission checks. The default session runs as this role.
pub const BOOTSTRAP_SUPERUSER: &str = "picklejar";

/// A bitmask of table privileges.
pub type PrivSet = u8;

/// `SELECT`: read rows.
pub const PRIV_SELECT: PrivSet = 1 << 0;
/// `INSERT`: add rows.
pub const PRIV_INSERT: PrivSet = 1 << 1;
/// `UPDATE`: modify rows.
pub const PRIV_UPDATE: PrivSet = 1 << 2;
/// `DELETE`: remove rows.
pub const PRIV_DELETE: PrivSet = 1 << 3;
/// `TRUNCATE`: empty the table.
pub const PRIV_TRUNCATE: PrivSet = 1 << 4;
/// Every table privilege.
pub const PRIV_ALL: PrivSet = PRIV_SELECT | PRIV_INSERT | PRIV_UPDATE | PRIV_DELETE | PRIV_TRUNCATE;

/// Translate a parsed [`Privilege`] into its bit(s).
#[must_use]
pub const fn priv_bits(p: Privilege) -> PrivSet {
    match p {
        Privilege::All => PRIV_ALL,
        Privilege::Select => PRIV_SELECT,
        Privilege::Insert => PRIV_INSERT,
        Privilege::Update => PRIV_UPDATE,
        Privilege::Delete => PRIV_DELETE,
        Privilege::Truncate => PRIV_TRUNCATE,
    }
}

/// The attributes of a role. Each is an independent on/off capability, so the
/// boolean-heavy shape mirrors PostgreSQL's `pg_authid` rather than a flag enum.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoleAttrs {
    /// Bypass every permission check.
    pub superuser: bool,
    /// May start a session (a user, as opposed to a pure group).
    pub login: bool,
    /// May create, alter, and drop other roles.
    pub createrole: bool,
    /// Skip row-level security policies.
    pub bypassrls: bool,
    /// Whether a login password has been set (the value itself is not stored;
    /// wire authentication is handled separately by the server).
    pub has_password: bool,
}

/// The roles, grants, memberships, and ownership of one database.
#[derive(Debug, Clone)]
pub struct SecurityCatalog {
    /// Role name -> attributes.
    roles: HashMap<String, RoleAttrs>,
    /// `(grantee, table)` -> granted privilege bits. The grantee `"public"`
    /// (lower-cased) is the implicit `PUBLIC` pseudo-role held by everyone.
    grants: HashMap<(String, String), PrivSet>,
    /// Role -> the groups it is a direct member of (it inherits their grants).
    member_of: HashMap<String, HashSet<String>>,
    /// Table -> owning role.
    owners: HashMap<String, String>,
}

impl Default for SecurityCatalog {
    fn default() -> Self {
        Self::new()
    }
}

impl SecurityCatalog {
    /// A new catalog containing only the bootstrap superuser.
    #[must_use]
    pub fn new() -> Self {
        let mut roles = HashMap::new();
        roles.insert(
            BOOTSTRAP_SUPERUSER.to_string(),
            RoleAttrs {
                superuser: true,
                login: true,
                createrole: true,
                bypassrls: true,
                has_password: false,
            },
        );
        Self {
            roles,
            grants: HashMap::new(),
            member_of: HashMap::new(),
            owners: HashMap::new(),
        }
    }

    /// Whether a role exists.
    #[must_use]
    pub fn role_exists(&self, role: &str) -> bool {
        self.roles.contains_key(role)
    }

    /// A role's attributes, if it exists.
    #[must_use]
    pub fn attrs(&self, role: &str) -> Option<&RoleAttrs> {
        self.roles.get(role)
    }

    /// Whether `role` is a superuser (an unknown role is not).
    #[must_use]
    pub fn is_superuser(&self, role: &str) -> bool {
        self.roles.get(role).is_some_and(|a| a.superuser)
    }

    /// Whether `role` may manage other roles (superuser or `CREATEROLE`).
    #[must_use]
    pub fn can_create_role(&self, role: &str) -> bool {
        self.roles
            .get(role)
            .is_some_and(|a| a.superuser || a.createrole)
    }

    /// Whether `role` bypasses row-level security (superuser or `BYPASSRLS`).
    #[must_use]
    pub fn can_bypass_rls(&self, role: &str) -> bool {
        self.roles
            .get(role)
            .is_some_and(|a| a.superuser || a.bypassrls)
    }

    /// Insert or replace a role with the given attributes.
    pub fn put_role(&mut self, name: &str, attrs: RoleAttrs) {
        self.roles.insert(name.to_string(), attrs);
    }

    /// Remove a role and every grant, membership, and ownership that names it.
    pub fn remove_role(&mut self, name: &str) {
        self.roles.remove(name);
        self.grants.retain(|(grantee, _), _| grantee != name);
        self.member_of.remove(name);
        for groups in self.member_of.values_mut() {
            groups.remove(name);
        }
        self.owners.retain(|_, owner| owner != name);
    }

    /// Grant `bits` on `table` to `grantee` (`"public"` for `PUBLIC`).
    pub fn grant(&mut self, grantee: &str, table: &str, bits: PrivSet) {
        *self
            .grants
            .entry((grantee.to_string(), table.to_string()))
            .or_insert(0) |= bits;
    }

    /// Revoke `bits` on `table` from `grantee`.
    pub fn revoke(&mut self, grantee: &str, table: &str, bits: PrivSet) {
        if let Some(cur) = self
            .grants
            .get_mut(&(grantee.to_string(), table.to_string()))
        {
            *cur &= !bits;
            if *cur == 0 {
                self.grants
                    .remove(&(grantee.to_string(), table.to_string()));
            }
        }
    }

    /// Make `member` a member of group `group` (inheriting its grants).
    pub fn add_member(&mut self, member: &str, group: &str) {
        self.member_of
            .entry(member.to_string())
            .or_default()
            .insert(group.to_string());
    }

    /// Remove `member` from `group`.
    pub fn remove_member(&mut self, member: &str, group: &str) {
        if let Some(groups) = self.member_of.get_mut(member) {
            groups.remove(group);
        }
    }

    /// Whether `member` belongs to `group`, directly or transitively.
    #[must_use]
    pub fn is_member_of(&self, member: &str, group: &str) -> bool {
        let mut stack = vec![member.to_string()];
        let mut seen = HashSet::new();
        while let Some(role) = stack.pop() {
            if !seen.insert(role.clone()) {
                continue;
            }
            if let Some(groups) = self.member_of.get(&role) {
                if groups.contains(group) {
                    return true;
                }
                stack.extend(groups.iter().cloned());
            }
        }
        false
    }

    /// Record `owner` as the owner of `table`.
    pub fn set_owner(&mut self, table: &str, owner: &str) {
        self.owners.insert(table.to_string(), owner.to_string());
    }

    /// Forget a table's ownership and every grant on it (on `DROP TABLE`).
    pub fn clear_owner(&mut self, table: &str) {
        self.owners.remove(table);
        self.grants.retain(|(_, t), _| t != table);
    }

    /// Rename a table's ownership record (on `ALTER TABLE ... RENAME TO`).
    pub fn rename_table(&mut self, from: &str, to: &str) {
        if let Some(owner) = self.owners.remove(from) {
            self.owners.insert(to.to_string(), owner);
        }
        // Move any grants attached to the old name.
        let moved: Vec<_> = self
            .grants
            .keys()
            .filter(|(_, t)| t == from)
            .cloned()
            .collect();
        for key in moved {
            if let Some(bits) = self.grants.remove(&key) {
                self.grants.insert((key.0, to.to_string()), bits);
            }
        }
    }

    /// The owner of `table`, if recorded.
    #[must_use]
    pub fn owner_of(&self, table: &str) -> Option<&str> {
        self.owners.get(table).map(String::as_str)
    }

    /// Whether `role` owns `table`.
    #[must_use]
    pub fn owns(&self, role: &str, table: &str) -> bool {
        self.owners.get(table).is_some_and(|o| o == role)
    }

    /// Whether `role` holds all of `needed` on `table`: as a superuser, as the
    /// owner, or through grants to the role itself, to `PUBLIC`, or to any group
    /// the role belongs to.
    #[must_use]
    pub fn has_privilege(&self, role: &str, table: &str, needed: PrivSet) -> bool {
        if self.is_superuser(role) || self.owns(role, table) {
            return true;
        }
        let mut held = self.direct_grant(role, table) | self.direct_grant("public", table);
        // Add every group the role transitively belongs to.
        let mut stack = vec![role.to_string()];
        let mut seen = HashSet::new();
        while let Some(r) = stack.pop() {
            if !seen.insert(r.clone()) {
                continue;
            }
            if let Some(groups) = self.member_of.get(&r) {
                for g in groups {
                    held |= self.direct_grant(g, table);
                    stack.push(g.clone());
                }
            }
        }
        held & needed == needed
    }

    /// The privilege bits granted directly to `grantee` on `table`.
    fn direct_grant(&self, grantee: &str, table: &str) -> PrivSet {
        self.grants
            .get(&(grantee.to_string(), table.to_string()))
            .copied()
            .unwrap_or(0)
    }

    // --- iteration for persistence ---

    /// All roles and their attributes.
    pub fn roles(&self) -> impl Iterator<Item = (&String, &RoleAttrs)> {
        self.roles.iter()
    }

    /// All `(grantee, table, bits)` grants.
    pub fn grants(&self) -> impl Iterator<Item = (&String, &String, &PrivSet)> {
        self.grants.iter().map(|((g, t), b)| (g, t, b))
    }

    /// All `(member, group)` membership edges.
    pub fn memberships(&self) -> impl Iterator<Item = (&String, &String)> {
        self.member_of
            .iter()
            .flat_map(|(m, gs)| gs.iter().map(move |g| (m, g)))
    }

    /// All `(table, owner)` pairs.
    pub fn owners(&self) -> impl Iterator<Item = (&String, &String)> {
        self.owners.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_superuser_bypasses_everything() {
        let c = SecurityCatalog::new();
        assert!(c.is_superuser(BOOTSTRAP_SUPERUSER));
        assert!(c.has_privilege(BOOTSTRAP_SUPERUSER, "t", PRIV_ALL));
        // An unknown role holds nothing.
        assert!(!c.has_privilege("nobody", "t", PRIV_SELECT));
    }

    #[test]
    fn direct_and_public_grants() {
        let mut c = SecurityCatalog::new();
        c.put_role("alice", RoleAttrs::default());
        assert!(!c.has_privilege("alice", "t", PRIV_SELECT));
        c.grant("alice", "t", PRIV_SELECT | PRIV_INSERT);
        assert!(c.has_privilege("alice", "t", PRIV_SELECT));
        assert!(c.has_privilege("alice", "t", PRIV_SELECT | PRIV_INSERT));
        assert!(!c.has_privilege("alice", "t", PRIV_DELETE));
        // A PUBLIC grant reaches everyone.
        c.put_role("bob", RoleAttrs::default());
        c.grant("public", "t", PRIV_SELECT);
        assert!(c.has_privilege("bob", "t", PRIV_SELECT));
        // Revoking the direct grant leaves only what PUBLIC carries.
        c.revoke("alice", "t", PRIV_INSERT);
        assert!(!c.has_privilege("alice", "t", PRIV_INSERT));
        assert!(c.has_privilege("alice", "t", PRIV_SELECT));
    }

    #[test]
    fn membership_inherits_grants_transitively() {
        let mut c = SecurityCatalog::new();
        c.put_role("readers", RoleAttrs::default());
        c.put_role("staff", RoleAttrs::default());
        c.put_role("alice", RoleAttrs::default());
        c.grant("readers", "t", PRIV_SELECT);
        // alice -> staff -> readers.
        c.add_member("staff", "readers");
        c.add_member("alice", "staff");
        assert!(c.is_member_of("alice", "readers"));
        assert!(c.has_privilege("alice", "t", PRIV_SELECT));
    }

    #[test]
    fn owner_has_all_privileges() {
        let mut c = SecurityCatalog::new();
        c.put_role("alice", RoleAttrs::default());
        c.set_owner("t", "alice");
        assert!(c.owns("alice", "t"));
        assert!(c.has_privilege("alice", "t", PRIV_ALL));
    }

    #[test]
    fn dropping_a_role_removes_its_grants() {
        let mut c = SecurityCatalog::new();
        c.put_role("alice", RoleAttrs::default());
        c.grant("alice", "t", PRIV_SELECT);
        c.set_owner("t2", "alice");
        c.remove_role("alice");
        assert!(!c.role_exists("alice"));
        assert!(!c.has_privilege("alice", "t", PRIV_SELECT));
        assert_eq!(c.owner_of("t2"), None);
    }
}
