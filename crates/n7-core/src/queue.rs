//! File de lecture : ordre normal ou aléatoire, répétition. Logique pure (aucun accès disque ou
//! périphérique audio), pour rester testable partout — les postes d'intégration continue n'ont
//! généralement pas de carte son, donc rien ici ne doit en dépendre.

use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RepeatMode {
    Off,
    All,
    One,
}

/// Un ordre d'affichage fixe (`order`, des identifiants de piste — en pratique un chemin de
/// fichier), un ordre de lecture éventuellement mélangé (`shuffled`, une permutation d'indices de
/// `order`), et une position courante dans cet ordre de lecture.
#[derive(Debug, Clone)]
pub struct PlaybackQueue {
    order: Vec<String>,
    shuffled: Vec<usize>,
    position: usize,
    shuffle: bool,
    repeat: RepeatMode,
}

impl PlaybackQueue {
    pub fn new(order: Vec<String>) -> Self {
        let shuffled = (0..order.len()).collect();
        Self {
            order,
            shuffled,
            position: 0,
            shuffle: false,
            repeat: RepeatMode::Off,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn repeat(&self) -> RepeatMode {
        self.repeat
    }

    pub fn shuffle_enabled(&self) -> bool {
        self.shuffle
    }

    pub fn set_repeat(&mut self, mode: RepeatMode) {
        self.repeat = mode;
    }

    /// Piste actuellement pointée (celle en cours de lecture, ou sur le point de démarrer).
    /// `None` seulement si la file est vide ou entièrement parcourue (répétition désactivée).
    pub fn current(&self) -> Option<&str> {
        self.shuffled
            .get(self.position)
            .and_then(|&i| self.order.get(i))
            .map(String::as_str)
    }

    /// Remplace entièrement la file (nouvel album ou nouvelle playlist choisie par l'utilisateur).
    /// Conserve l'état "aléatoire" / "répéter" mais repart de la première piste.
    pub fn set_order(&mut self, order: Vec<String>) {
        self.order = order;
        self.shuffled = (0..self.order.len()).collect();
        self.position = 0;
        if self.shuffle {
            self.shuffled.shuffle(&mut rand::thread_rng());
        }
    }

    /// Active ou désactive la lecture aléatoire. En l'activant, la piste courante reste en tête
    /// de la nouvelle file mélangée : elle ne "saute" jamais au moment où on l'active. En la
    /// désactivant, on retrouve l'ordre d'affichage normal, toujours positionné sur la même piste.
    pub fn set_shuffle(&mut self, on: bool) {
        if on == self.shuffle {
            return;
        }
        self.shuffle = on;
        if on {
            self.reshuffle_keeping_current();
        } else {
            // Indice d'origine de la piste courante, lu AVANT d'écraser `shuffled` : c'est cet
            // ancien mappage qui sait à quelle piste "position" correspond réellement.
            let original_index = self.shuffled.get(self.position).copied().unwrap_or(0);
            self.shuffled = (0..self.order.len()).collect();
            self.position = original_index.min(self.shuffled.len().saturating_sub(1));
        }
    }

    fn reshuffle_keeping_current(&mut self) {
        let current_original_index = self.shuffled.get(self.position).copied();
        let mut rest: Vec<usize> = (0..self.order.len())
            .filter(|&i| Some(i) != current_original_index)
            .collect();
        rest.shuffle(&mut rand::thread_rng());
        self.shuffled = match current_original_index {
            Some(current) => {
                let mut v = Vec::with_capacity(self.order.len());
                v.push(current);
                v.extend(rest);
                v
            }
            None => rest,
        };
        self.position = 0;
    }

    /// Positionne la file sur une piste précise (clic direct dans la liste des pistes).
    /// Renvoie `false` si cette piste n'est pas dans la file courante.
    pub fn jump_to(&mut self, track_id: &str) -> bool {
        let Some(original_index) = self.order.iter().position(|t| t == track_id) else {
            return false;
        };
        let Some(pos_in_shuffled) = self.shuffled.iter().position(|&i| i == original_index) else {
            return false;
        };
        self.position = pos_in_shuffled;
        true
    }

    /// Avance d'une piste. `None` = fin de file (rien à jouer ensuite, répétition désactivée).
    pub fn next(&mut self) -> Option<&str> {
        if self.order.is_empty() {
            return None;
        }
        if self.repeat == RepeatMode::One {
            return self.current();
        }
        if self.position + 1 < self.shuffled.len() {
            self.position += 1;
        } else if self.repeat == RepeatMode::All {
            if self.shuffle {
                self.shuffled.shuffle(&mut rand::thread_rng());
            }
            self.position = 0;
        } else {
            self.position = self.shuffled.len(); // sentinelle : file terminée
            return None;
        }
        self.current()
    }

    /// Recule d'une piste. Ne descend jamais avant le début, sauf en répétition "Tout" (boucle
    /// alors sur la dernière piste).
    pub fn previous(&mut self) -> Option<&str> {
        if self.order.is_empty() {
            return None;
        }
        if self.repeat == RepeatMode::One {
            return self.current();
        }
        if self.position > 0 && self.position < self.shuffled.len() {
            self.position -= 1;
        } else if self.repeat == RepeatMode::All {
            self.position = self.shuffled.len().saturating_sub(1);
        } else {
            self.position = 0;
        }
        self.current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn ids(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("track-{i}")).collect()
    }

    #[test]
    fn new_queue_starts_on_the_first_track() {
        let q = PlaybackQueue::new(ids(3));
        assert_eq!(q.current(), Some("track-0"));
        assert_eq!(q.len(), 3);
        assert!(!q.is_empty());
    }

    #[test]
    fn empty_queue_never_panics() {
        let mut q = PlaybackQueue::new(Vec::new());
        assert!(q.is_empty());
        assert_eq!(q.current(), None);
        assert_eq!(q.next(), None);
        assert_eq!(q.previous(), None);
        assert!(!q.jump_to("track-0"));
    }

    #[test]
    fn next_advances_in_order_without_repeat() {
        let mut q = PlaybackQueue::new(ids(3));
        assert_eq!(q.next(), Some("track-1"));
        assert_eq!(q.next(), Some("track-2"));
        assert_eq!(q.next(), None, "fin de file sans répétition");
        assert_eq!(q.current(), None);
    }

    #[test]
    fn repeat_all_wraps_to_the_start() {
        let mut q = PlaybackQueue::new(ids(2));
        q.set_repeat(RepeatMode::All);
        assert_eq!(q.next(), Some("track-1"));
        assert_eq!(q.next(), Some("track-0"), "boucle sur la première piste");
    }

    #[test]
    fn repeat_one_replays_the_same_track() {
        let mut q = PlaybackQueue::new(ids(3));
        q.jump_to("track-1");
        q.set_repeat(RepeatMode::One);
        assert_eq!(q.next(), Some("track-1"));
        assert_eq!(q.next(), Some("track-1"));
        assert_eq!(q.previous(), Some("track-1"));
    }

    #[test]
    fn previous_steps_back_and_clamps_without_repeat() {
        let mut q = PlaybackQueue::new(ids(3));
        q.jump_to("track-2");
        assert_eq!(q.previous(), Some("track-1"));
        assert_eq!(q.previous(), Some("track-0"));
        assert_eq!(q.previous(), Some("track-0"), "reste sur la première piste");
    }

    #[test]
    fn previous_wraps_to_the_end_with_repeat_all() {
        let mut q = PlaybackQueue::new(ids(3));
        q.set_repeat(RepeatMode::All);
        assert_eq!(q.previous(), Some("track-2"));
    }

    #[test]
    fn jump_to_unknown_track_is_a_no_op() {
        let mut q = PlaybackQueue::new(ids(2));
        assert!(!q.jump_to("does-not-exist"));
        assert_eq!(q.current(), Some("track-0"));
    }

    #[test]
    fn shuffle_keeps_the_current_track_first_then_covers_every_track_once() {
        let mut q = PlaybackQueue::new(ids(5));
        q.jump_to("track-2");
        q.set_shuffle(true);
        assert_eq!(q.current(), Some("track-2"), "la piste en cours ne saute pas");

        let mut visited = HashSet::new();
        visited.insert(q.current().unwrap().to_string());
        for _ in 0..4 {
            let t = q.next().expect("encore des pistes avant la fin").to_string();
            visited.insert(t);
        }
        assert_eq!(visited.len(), 5, "chaque piste vue exactement une fois");
        assert_eq!(q.next(), None, "fin de file sans répétition, même mélangée");
    }

    #[test]
    fn disabling_shuffle_restores_normal_order_on_the_same_track() {
        let mut q = PlaybackQueue::new(ids(4));
        q.set_shuffle(true);
        q.jump_to("track-3");
        q.set_shuffle(false);
        assert_eq!(q.current(), Some("track-3"));
        assert_eq!(q.next(), None, "track-3 est la dernière piste en ordre normal");
    }

    #[test]
    fn set_order_resets_position_to_the_start() {
        let mut q = PlaybackQueue::new(ids(3));
        q.jump_to("track-2");
        q.set_order(ids(2));
        assert_eq!(q.len(), 2);
        assert_eq!(q.current(), Some("track-0"));
    }
}
